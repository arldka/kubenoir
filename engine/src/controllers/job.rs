//! The Job controller: runs pods until `completions` of them have succeeded, retries failures
//! (flat 10 s apart, like the kubelet's restarts) and gives up when more than `backoffLimit`
//! have failed or the deadline has passed (V17).

use serde_json::{json, Value};

use super::pods::{delete_pod, owner_ref};
use super::Env;
use crate::clock;
use crate::events::{self, EventType, Involved, Source};
use crate::names;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

/// A Job starts a replacement for a failed pod this long after the failure.
pub const RETRY_SECS: i64 = 10;
/// The row under which a Job asks to be woken (pods cannot have a `/` in their name).
fn wake_key(name: &str) -> String {
    format!("job/{name}")
}

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let pods = env.store.list_all("Pod")?;
    let mut changed = false;
    for job in env.store.list_all("Job")? {
        changed |= sync(env, job, &pods)?;
    }
    Ok(changed)
}

fn int(value: &Value, default: i64) -> i64 {
    value.as_i64().unwrap_or(default)
}

fn phase(pod: &Value) -> &str {
    pod["status"]["phase"].as_str().unwrap_or("Pending")
}

/// When a pod's containers last finished, if they did.
fn finished_at(pod: &Value) -> Option<i64> {
    pod["status"]["containerStatuses"]
        .as_array()?
        .iter()
        .filter_map(|c| c["state"]["terminated"]["finishedAt"].as_str())
        .filter_map(|t| serde_json::from_value::<k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>(json!(t)).ok())
        .map(|t| clock::secs(&t))
        .max()
}

fn restarts(pod: &Value) -> i64 {
    pod["status"]["containerStatuses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c["restartCount"].as_i64().unwrap_or(0))
        .sum()
}

fn condition<'a>(job: &'a Value, kind: &str) -> Option<&'a Value> {
    job["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == kind && c["status"] == "True")
}

fn sync(env: &Env<'_>, mut job: Value, all_pods: &[Value]) -> Result<bool, StoreError> {
    let namespace = job["metadata"]["namespace"]
        .as_str()
        .unwrap_or(DEFAULT_NAMESPACE)
        .to_string();
    let name = job["metadata"]["name"].as_str().unwrap_or_default().to_string();
    let uid = job["metadata"]["uid"].as_str().unwrap_or_default().to_string();
    let before = job.clone();
    let spec = job["spec"].clone();

    let owned: Vec<&Value> = all_pods
        .iter()
        .filter(|p| {
            p["metadata"]["namespace"].as_str().unwrap_or(DEFAULT_NAMESPACE) == namespace
                && p["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|o| o.iter().any(|r| r["uid"] == uid.as_str()))
        })
        .collect();
    let live = |p: &&&Value| p["metadata"].get("deletionTimestamp").is_none();
    let active: Vec<&&Value> = owned
        .iter()
        .filter(live)
        .filter(|p| !matches!(phase(p), "Succeeded" | "Failed"))
        .collect();
    let succeeded = owned.iter().filter(|p| phase(p) == "Succeeded").count() as i64;
    let failed_pods = owned.iter().filter(|p| phase(p) == "Failed").count() as i64;
    // Containers restarted in place (`restartPolicy: OnFailure`) count as failures too.
    let failed = failed_pods + owned.iter().map(|p| restarts(p)).sum::<i64>();

    let started = job["status"]["startTime"]
        .as_str()
        .and_then(|t| serde_json::from_value::<k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>(json!(t)).ok())
        .map(|t| clock::secs(&t))
        .unwrap_or_else(|| {
            job["metadata"]["creationTimestamp"]
                .as_str()
                .and_then(|t| {
                    serde_json::from_value::<k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>(json!(t)).ok()
                })
                .map_or(env.now, |t| clock::secs(&t))
        });

    let involved = Involved {
        kind: "Job",
        api_version: "batch/v1",
        namespace: &namespace,
        name: &name,
        uid: &uid,
        field_path: None,
    };
    let source = Source {
        component: "job-controller",
        host: None,
    };
    let mut changed = false;
    let mut wake: Option<i64> = None;

    let finished = condition(&job, "Complete").is_some() || condition(&job, "Failed").is_some();
    let completions = int(&spec["completions"], 1);
    let backoff_limit = int(&spec["backoffLimit"], 6);
    let last_failure = owned
        .iter()
        .filter(|p| phase(p) == "Failed")
        .filter_map(|p| finished_at(p))
        .max();
    let mut verdict: Option<(&str, &str, &str, i64)> = None; // type, reason, message, at
    if !finished {
        let suspended = spec["suspend"].as_bool().unwrap_or(false);
        if succeeded >= completions {
            let at = owned
                .iter()
                .filter(|p| phase(p) == "Succeeded")
                .filter_map(|p| finished_at(p))
                .max()
                .unwrap_or(env.now);
            verdict = Some(("Complete", "", "", at));
        } else if failed > backoff_limit {
            let at = last_failure.unwrap_or(env.now);
            verdict = Some((
                "Failed",
                "BackoffLimitExceeded",
                "Job has reached the specified backoff limit",
                at,
            ));
        } else if let Some(deadline) = spec["activeDeadlineSeconds"].as_i64() {
            if env.now >= started + deadline {
                verdict = Some((
                    "Failed",
                    "DeadlineExceeded",
                    "Job was active longer than specified deadline",
                    started + deadline,
                ));
            } else {
                wake = Some(started + deadline);
            }
        }
        if verdict.is_none() && !suspended {
            let want = (int(&spec["parallelism"], 1).min(completions - succeeded) - active.len() as i64).max(0);
            let not_before = last_failure.map(|f| f + RETRY_SECS).filter(|_| active.is_empty());
            if want > 0 {
                match not_before.filter(|t| *t > env.now) {
                    Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
                    None => {
                        for _ in 0..want {
                            let pod = create_pod(env, &job, &namespace, &name, &uid)?;
                            events::record(
                                env.store,
                                &involved,
                                EventType::Normal,
                                "SuccessfulCreate",
                                &format!("Created pod: {pod}"),
                                &source,
                                env.now,
                            )?;
                        }
                        changed = true;
                    }
                }
            }
        }
    }

    let mut finish = finished;
    if let Some((kind, reason, message, at)) = verdict {
        finish = true;
        let conditions = job["status"]["conditions"].as_array().cloned().unwrap_or_default();
        let mut conditions = conditions;
        let mut entry = json!({"type": kind, "status": "True", "lastProbeTime": clock::format(at),
                               "lastTransitionTime": clock::format(at)});
        if !reason.is_empty() {
            entry["reason"] = json!(reason);
            entry["message"] = json!(message);
        }
        conditions.push(entry);
        job["status"]["conditions"] = json!(conditions);
        if kind == "Complete" {
            job["status"]["completionTime"] = json!(clock::format(at));
            events::record(
                env.store,
                &involved,
                EventType::Normal,
                "Completed",
                "Job completed",
                &source,
                at,
            )?;
        } else {
            events::record(env.store, &involved, EventType::Warning, reason, message, &source, at)?;
        }
    }
    // A failed Job stops its pods.
    if condition(&job, "Failed").is_some() {
        for pod in active.iter() {
            let typed: k8s_openapi::api::core::v1::Pod = serde_json::from_value((***pod).clone())?;
            delete_pod(env, &typed, false)?;
            changed = true;
        }
    }

    let (active_now, ready) = if finish && condition(&job, "Failed").is_none() {
        (0, 0)
    } else {
        let fresh = env.store.list("Pod", &namespace)?;
        let mine: Vec<&Value> = fresh
            .iter()
            .filter(|p| {
                p["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|o| o.iter().any(|r| r["uid"] == uid.as_str()))
            })
            .filter(|p| p["metadata"].get("deletionTimestamp").is_none() && !matches!(phase(p), "Succeeded" | "Failed"))
            .collect();
        let ready = mine
            .iter()
            .filter(|p| {
                p["status"]["conditions"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|c| c["type"] == "Ready" && c["status"] == "True"))
            })
            .count();
        (mine.len() as i64, ready as i64)
    };
    if !job["status"].is_object() {
        job["status"] = json!({});
    }
    let status = job["status"].as_object_mut().expect("status is an object");
    if job_status_dirty(status, active_now, succeeded, failed_pods.max(failed), ready, started) {
        changed = true;
    }
    if job != before {
        env.store.update("Job", &namespace, &name, &job)?;
        changed = true;
    }
    match wake {
        Some(at) if !finish => {
            env.store
                .runtime_put(&namespace, &wake_key(&name), "#job", &json!({}), Some(at))?;
        }
        _ => env.store.purge_pod_state(&namespace, &wake_key(&name))?,
    }
    Ok(changed)
}

/// Write the counters into `status`; whether anything changed is judged by the caller comparing
/// the whole object, so this only reports whether it set something new.
fn job_status_dirty(
    status: &mut serde_json::Map<String, Value>,
    active: i64,
    succeeded: i64,
    failed: i64,
    ready: i64,
    started: i64,
) -> bool {
    let mut set = |key: &str, value: Value| {
        let dirty = status.get(key) != Some(&value);
        status.insert(key.into(), value);
        dirty
    };
    let mut dirty = false;
    dirty |= set("active", json!(active));
    dirty |= set("succeeded", json!(succeeded));
    dirty |= set("failed", json!(failed));
    dirty |= set("ready", json!(ready));
    dirty |= set("startTime", json!(clock::format(started)));
    dirty |= set("terminating", json!(0));
    dirty
}

fn create_pod(env: &Env<'_>, job: &Value, namespace: &str, name: &str, uid: &str) -> Result<String, StoreError> {
    let pod_name = loop {
        let candidate = format!("{name}-{}", names::pod_suffix(env.store.seed(), env.store.next_id()));
        if env.store.get("Pod", namespace, &candidate)?.is_none() {
            break candidate;
        }
    };
    let template = &job["spec"]["template"];
    let mut labels = template["metadata"]["labels"].as_object().cloned().unwrap_or_default();
    for (key, value) in [
        ("batch.kubernetes.io/controller-uid", uid),
        ("batch.kubernetes.io/job-name", name),
        ("controller-uid", uid),
        ("job-name", name),
    ] {
        labels.insert(key.into(), json!(value));
    }
    let mut metadata = json!({
        "name": pod_name,
        "generateName": format!("{name}-"),
        "namespace": namespace,
        "creationTimestamp": clock::format(env.now),
        "labels": labels,
        "ownerReferences": [owner_ref("batch/v1", "Job", name, uid)],
    });
    if let Some(annotations) = template["metadata"]["annotations"].as_object() {
        metadata["annotations"] = json!(annotations);
    }
    let mut spec = template["spec"].clone();
    for (key, default) in [
        ("dnsPolicy", json!("ClusterFirst")),
        ("serviceAccountName", json!("default")),
        ("terminationGracePeriodSeconds", json!(30)),
    ] {
        spec.as_object_mut().map(|o| o.entry(key).or_insert(default));
    }
    let pod = json!({"apiVersion": "v1", "kind": "Pod", "metadata": metadata, "spec": spec});
    env.store.insert("Pod", "v1", namespace, &pod_name, &pod)?;
    Ok(pod_name)
}
