//! `kubectl logs`.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;

use crate::clock;
use crate::controllers::{select_log_instance, LogSelection};
use crate::parser::{LogsArgs, ResourceKind};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::{ClusterStore, StoreError};

/// User-facing failures come back as `Ok(kubectl error text)`, like the rest of the executor;
/// `Err` is reserved for a broken store.
pub fn logs(args: &LogsArgs, store: &ClusterStore) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);

    let mut notices: Vec<String> = Vec::new();
    let pod_name = match args.resource {
        ResourceKind::Deployment => match pick_deployment_pod(store, namespace, &args.name)? {
            Ok((name, found)) => {
                if found > 1 {
                    notices.push(format!("Found {found} pods, using pod/{name}"));
                }
                name
            }
            Err(message) => return Ok(message),
        },
        _ => args.name.clone(),
    };

    let Some(manifest) = store.get("Pod", namespace, &pod_name)? else {
        return Ok(format!("Error from server (NotFound): pods \"{pod_name}\" not found"));
    };
    let pod: Pod = serde_json::from_value(manifest)?;
    let containers: Vec<&str> = pod
        .spec
        .iter()
        .flat_map(|s| s.containers.iter().map(|c| c.name.as_str()))
        .collect();

    let container = match &args.container {
        Some(c) if containers.contains(&c.as_str()) => c.as_str(),
        Some(c) => {
            return Ok(format!(
                "Error from server (BadRequest): container {c} is not valid for pod {pod_name}"
            ));
        }
        None => {
            let Some(first) = containers.first() else {
                return Ok(format!(
                    "Error from server (BadRequest): pod {pod_name} has no containers"
                ));
            };
            if containers.len() > 1 {
                notices.push(format!(
                    "Defaulted container \"{first}\" out of: {}",
                    containers.join(", ")
                ));
            }
            first
        }
    };

    let selection = select_log_instance(store, namespace, &pod_name, container, args.previous)?;
    let body = match selection {
        // Static pods have no simulated process, hence nothing was ever written.
        None => String::new(),
        Some(LogSelection::WaitingToStart) => {
            return Ok(format!(
                "Error from server (BadRequest): container \"{container}\" in pod \"{pod_name}\" is waiting to start: ContainerCreating"
            ));
        }
        Some(LogSelection::NoPrevious) => {
            return Ok(format!(
                "Error from server (BadRequest): previous terminated container \"{container}\" in pod \"{pod_name}\" not found"
            ));
        }
        Some(LogSelection::Instance(instance)) => {
            let mut lines = store.read_logs(namespace, &pod_name, container, instance)?;
            if let Some(n) = args.tail {
                lines.drain(..lines.len().saturating_sub(n));
            }
            lines
                .iter()
                .map(|l| {
                    if args.timestamps {
                        format!("{}.000000000Z {}", &clock::format(l.ts)[..19], l.line)
                    } else {
                        l.line.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    };

    if !body.is_empty() {
        notices.push(body);
    }
    Ok(notices.join("\n"))
}

/// The pod `kubectl logs deploy/NAME` reads from: ready pods first, then oldest. Returns the
/// chosen pod and how many candidates there were, or the kubectl error text.
fn pick_deployment_pod(
    store: &ClusterStore,
    namespace: &str,
    name: &str,
) -> Result<Result<(String, usize), String>, StoreError> {
    let Some(manifest) = store.get("Deployment", namespace, name)? else {
        return Ok(Err(format!(
            "Error from server (NotFound): deployments.apps \"{name}\" not found"
        )));
    };
    let deployment: Deployment = serde_json::from_value(manifest)?;
    let selector = deployment
        .spec
        .and_then(|s| s.selector.match_labels)
        .unwrap_or_default();

    let mut candidates: Vec<Pod> = Vec::new();
    for manifest in store.list("Pod", namespace)? {
        let pod: Pod = serde_json::from_value(manifest)?;
        let labels = pod.metadata.labels.clone().unwrap_or_default();
        if pod.metadata.deletion_timestamp.is_none() && selector.iter().all(|(k, v)| labels.get(k) == Some(v)) {
            candidates.push(pod);
        }
    }
    candidates.sort_by_key(|p| {
        let ready = p
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"));
        (
            !ready,
            p.metadata.creation_timestamp.as_ref().map_or(0, clock::secs),
            p.metadata.name.clone(),
        )
    });
    match candidates.first() {
        Some(pod) => Ok(Ok((pod.metadata.name.clone().unwrap_or_default(), candidates.len()))),
        None => Ok(Err(format!("error: no pods found for deployment/{name}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::sim::Engine;

    /// `app` crashes 1 s after starting; `sidecar` just runs. Pulling takes 2 s,
    /// so `app` starts at +2 s, crashes at +3 s and restarts at +13 s.
    fn engine() -> Engine {
        crate::scenario::inline::engine(
            r#"{
            "id": "l",
            "images": {
                "app:1": {"app": "script@0.1.0", "config":
                    {"logs": [{"line": "starting"}, {"at": 1, "line": "FATAL DATABASE_URL not set"}], "exit": {"at": 1, "code": 1}}},
                "side:1": {"app": "script@0.1.0", "config": {"logs": [{"line": "sidecar up"}]}}
            },
            "cluster": {"now": "2026-10-27T18:30:00Z", "nodes": ["n1"], "pods": [{
                "apiVersion": "v1", "kind": "Pod", "metadata": {"name": "p"},
                "spec": {"containers": [{"name": "app", "image": "app:1"}, {"name": "sidecar", "image": "side:1"}]}
            }]}
        }"#,
        )
    }

    const DEFAULTED: &str = "Defaulted container \"app\" out of: app, sidecar";

    #[test]
    fn waiting_to_start_is_an_error() {
        let mut e = engine();
        assert_eq!(
            e.eval("kubectl logs p -c app"),
            "Error from server (BadRequest): container \"app\" in pod \"p\" is waiting to start: ContainerCreating"
        );
    }

    #[test]
    fn crashed_container_shows_its_last_run_with_or_without_previous() {
        let mut e = engine();
        e.advance(3); // crashed, in CrashLoopBackOff
        let expected = "starting\nFATAL DATABASE_URL not set";
        assert_eq!(e.eval("kubectl logs p -c app"), expected);
        assert_eq!(e.eval("kubectl logs p -c app --previous"), expected);
        assert_eq!(e.eval("kubectl logs p -c app -p"), expected);
    }

    #[test]
    fn restarted_container_shows_the_new_run_and_previous_shows_the_old_one() {
        let mut e = engine();
        e.advance(13); // restart #1 just started: only the first line of the new run exists
        assert_eq!(e.eval("kubectl logs p -c app"), "starting");
        assert_eq!(
            e.eval("kubectl logs p -c app -p"),
            "starting\nFATAL DATABASE_URL not set"
        );
        e.advance(1); // crashed again
        assert_eq!(e.eval("kubectl logs p -c app"), "starting\nFATAL DATABASE_URL not set");
    }

    #[test]
    fn previous_without_an_earlier_run_is_not_found() {
        let mut e = engine();
        e.advance(2); // first run in progress
        assert_eq!(
            e.eval("kubectl logs p -c app -p"),
            "Error from server (BadRequest): previous terminated container \"app\" in pod \"p\" not found"
        );
        assert_eq!(e.eval("kubectl logs p -c sidecar"), "sidecar up");
    }

    #[test]
    fn tail_limits_to_the_last_lines() {
        let mut e = engine();
        e.advance(3);
        assert_eq!(e.eval("kubectl logs p -c app --tail 1"), "FATAL DATABASE_URL not set");
        assert_eq!(e.eval("kubectl logs p -c app --tail=0"), "");
        assert_eq!(
            e.eval("kubectl logs p -c app --tail 99"),
            "starting\nFATAL DATABASE_URL not set"
        );
        assert_eq!(
            e.eval("kubectl logs p -c app --tail -1"),
            "starting\nFATAL DATABASE_URL not set"
        );
    }

    #[test]
    fn timestamps_use_the_time_each_line_was_written() {
        let mut e = engine();
        e.advance(3);
        assert_eq!(
            e.eval("kubectl logs p -c app --timestamps"),
            "2026-10-27T18:30:02.000000000Z starting\n2026-10-27T18:30:03.000000000Z FATAL DATABASE_URL not set"
        );
    }

    #[test]
    fn multi_container_pods_default_to_the_first_container_with_a_notice() {
        let mut e = engine();
        e.advance(3);
        assert_eq!(
            e.eval("kubectl logs p"),
            format!("{DEFAULTED}\nstarting\nFATAL DATABASE_URL not set")
        );
        assert_eq!(
            e.eval("kubectl logs pod/p --tail 1"),
            format!("{DEFAULTED}\nFATAL DATABASE_URL not set")
        );
    }

    #[test]
    fn errors_for_unknown_pods_and_containers() {
        let mut e = engine();
        e.advance(3);
        assert_eq!(
            e.eval("kubectl logs nope"),
            "Error from server (NotFound): pods \"nope\" not found"
        );
        assert_eq!(
            e.eval("kubectl logs p -c nope"),
            "Error from server (BadRequest): container nope is not valid for pod p"
        );
        assert_eq!(
            e.eval("kubectl logs p -c app -n ghost"),
            "Error from server (NotFound): pods \"p\" not found"
        );
    }

    #[test]
    fn static_pods_have_no_output() {
        let mut e = crate::scenario::inline::repo("sample-crashloop");
        assert_eq!(e.eval("kubectl logs web-7d9f8b6c5-x2k4p"), "");
    }
}
