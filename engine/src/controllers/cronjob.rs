//! The CronJob controller: creates a Job whenever the schedule says so, honoring the concurrency
//! policy and the history limits (V17). Times are UTC.

use serde_json::{json, Value};

use super::pods::owner_ref;
use super::Env;
use crate::clock;
use crate::cron;
use crate::events::{self, EventType, Involved, Source};
use crate::mutate::delete_object;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

fn wake_key(name: &str) -> String {
    format!("cronjob/{name}")
}

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut changed = false;
    for cronjob in env.store.list_all("CronJob")? {
        changed |= sync(env, cronjob)?;
    }
    Ok(changed)
}

fn time(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(|t| serde_json::from_value::<k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>(json!(t)).ok())
        .map(|t| clock::secs(&t))
}

fn finished(job: &Value) -> Option<&'static str> {
    let has = |kind: &str| {
        job["status"]["conditions"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["type"] == kind && c["status"] == "True"))
    };
    if has("Complete") {
        Some("Complete")
    } else if has("Failed") {
        Some("Failed")
    } else {
        None
    }
}

fn sync(env: &Env<'_>, mut cronjob: Value) -> Result<bool, StoreError> {
    let namespace = cronjob["metadata"]["namespace"]
        .as_str()
        .unwrap_or(DEFAULT_NAMESPACE)
        .to_string();
    let name = cronjob["metadata"]["name"].as_str().unwrap_or_default().to_string();
    let uid = cronjob["metadata"]["uid"].as_str().unwrap_or_default().to_string();
    let before = cronjob.clone();
    let spec = cronjob["spec"].clone();
    let Ok(schedule) = cron::parse(spec["schedule"].as_str().unwrap_or_default()) else {
        return Ok(false);
    };
    let involved = Involved {
        kind: "CronJob",
        api_version: "batch/v1",
        namespace: &namespace,
        name: &name,
        uid: &uid,
        field_path: None,
    };
    let source = Source {
        component: "cronjob-controller",
        host: None,
    };

    let jobs_of = |env: &Env<'_>| -> Result<Vec<Value>, StoreError> {
        Ok(env
            .store
            .list("Job", &namespace)?
            .into_iter()
            .filter(|j| {
                j["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|o| o.iter().any(|r| r["uid"] == uid.as_str()))
            })
            .collect())
    };
    let mut jobs = jobs_of(env)?;
    let mut changed = false;

    let created = time(&cronjob["metadata"]["creationTimestamp"]).unwrap_or(env.now);
    let last = time(&cronjob["status"]["lastScheduleTime"]).unwrap_or(created);
    let suspended = spec["suspend"].as_bool().unwrap_or(false);
    if !suspended {
        if let Some(due) = schedule.last_between(last, env.now) {
            let active: Vec<&Value> = jobs.iter().filter(|j| finished(j).is_none()).collect();
            let policy = spec["concurrencyPolicy"].as_str().unwrap_or("Allow");
            if policy == "Forbid" && !active.is_empty() {
                // Not now; it is tried again when the running Job is done.
            } else {
                if policy == "Replace" {
                    for job in active {
                        let job_name = job["metadata"]["name"].as_str().unwrap_or_default();
                        delete_object(env, "Job", &namespace, job_name, false)?;
                    }
                }
                let job_name = format!("{name}-{}", due / 60);
                if env.store.get("Job", &namespace, &job_name)?.is_none() {
                    let mut template = spec["jobTemplate"].clone();
                    let mut metadata = template["metadata"].take();
                    if !metadata.is_object() {
                        metadata = json!({});
                    }
                    metadata["name"] = json!(job_name);
                    metadata["namespace"] = json!(namespace);
                    metadata["creationTimestamp"] = json!(clock::format(due));
                    metadata["ownerReferences"] = json!([owner_ref("batch/v1", "CronJob", &name, &uid)]);
                    metadata["annotations"]["batch.kubernetes.io/cronjob-scheduled-timestamp"] =
                        json!(clock::format(due));
                    let job = json!({"apiVersion": "batch/v1", "kind": "Job", "metadata": metadata, "spec": template["spec"]});
                    env.store.insert("Job", "batch/v1", &namespace, &job_name, &job)?;
                    events::record(
                        env.store,
                        &involved,
                        EventType::Normal,
                        "SuccessfulCreate",
                        &format!("Created job {job_name}"),
                        &source,
                        due,
                    )?;
                    changed = true;
                }
                cronjob["status"]["lastScheduleTime"] = json!(clock::format(due));
                jobs = jobs_of(env)?;
            }
        }
    }

    // History: keep the newest few finished Jobs of each kind.
    let keep = |outcome: &str, limit: i64, jobs: &[Value]| -> Result<bool, StoreError> {
        let mut done: Vec<&Value> = jobs.iter().filter(|j| finished(j) == Some(outcome)).collect();
        done.sort_by_key(|j| time(&j["metadata"]["creationTimestamp"]).unwrap_or(0));
        let mut removed = false;
        while done.len() as i64 > limit.max(0) {
            let oldest = done.remove(0);
            delete_object(
                env,
                "Job",
                &namespace,
                oldest["metadata"]["name"].as_str().unwrap_or_default(),
                true,
            )?;
            removed = true;
        }
        Ok(removed)
    };
    let history = (
        spec["successfulJobsHistoryLimit"].as_i64().unwrap_or(3),
        spec["failedJobsHistoryLimit"].as_i64().unwrap_or(1),
    );
    if keep("Complete", history.0, &jobs)? | keep("Failed", history.1, &jobs)? {
        changed = true;
        jobs = jobs_of(env)?;
    }

    let active: Vec<Value> = jobs
        .iter()
        .filter(|j| finished(j).is_none())
        .map(|j| {
            json!({"kind": "Job", "namespace": namespace, "name": j["metadata"]["name"],
                        "uid": j["metadata"]["uid"], "apiVersion": "batch/v1"})
        })
        .collect();
    cronjob["status"]["active"] = json!(active);
    if let Some(at) = jobs
        .iter()
        .filter(|j| finished(j) == Some("Complete"))
        .filter_map(|j| time(&j["status"]["completionTime"]))
        .max()
    {
        cronjob["status"]["lastSuccessfulTime"] = json!(clock::format(at));
    }
    if cronjob != before {
        env.store.update("CronJob", &namespace, &name, &cronjob)?;
        changed = true;
    }
    match (suspended, schedule.next_after(env.now)) {
        (false, Some(next)) => {
            env.store
                .runtime_put(&namespace, &wake_key(&name), "#cron", &json!({}), Some(next))?;
        }
        _ => env.store.purge_pod_state(&namespace, &wake_key(&name))?,
    }
    Ok(changed)
}
