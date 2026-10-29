//! The connectivity oracle: what happens when a pod connects to `host:port`, worked out from the
//! cluster state alone (V8). No sockets and no packets: DNS search paths, Service ports, ready
//! endpoints, and whether the target pod listens on the port.
//!
//! Callers pass the state of the store at the moment the container started (V32, C14); the
//! function itself keeps no state and has no randomness.

use std::collections::BTreeSet;

use serde_json::Value;

mod policy;

use crate::store::{ClusterStore, StoreError};

const CLUSTER_DOMAIN: &str = "cluster.local";
/// Names with fewer dots than this are tried against the search path first (`ndots:5`).
const NDOTS: usize = 5;
/// ExternalName chains longer than this are a loop.
const MAX_ALIASES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The name does not resolve.
    NxDomain,
    /// Something answered "nothing here": no ready endpoint, or the pod does not listen.
    Refused,
    /// Nobody answered: the port is not one the Service exposes.
    Timeout,
    /// A pod accepted the connection.
    Connected { namespace: String, pod: String, port: i64 },
    /// The name points outside the cluster; what answers there is not simulated.
    External { host: String },
}

/// Connect from the pod `source` (a Pod manifest: its namespace and labels matter) to
/// `host:port`. `listens(pod, port)` says whether a pod's containers accept connections on a port.
pub fn connect(
    store: &ClusterStore,
    source: &Value,
    host: &str,
    port: i64,
    listens: &dyn Fn(&Value, i64) -> bool,
) -> Result<Outcome, StoreError> {
    let mut host = host.trim_end_matches('.').to_ascii_lowercase();
    let mut namespace = source["metadata"]["namespace"]
        .as_str()
        .unwrap_or("default")
        .to_string();
    for _ in 0..MAX_ALIASES {
        if host.parse::<std::net::Ipv4Addr>().is_ok() {
            return by_address(store, source, &host, port, listens);
        }
        let Some((service, service_namespace)) = lookup(store, &namespace, &host)? else {
            return Ok(Outcome::NxDomain);
        };
        if service["spec"]["type"].as_str() == Some("ExternalName") {
            let target = service["spec"]["externalName"]
                .as_str()
                .unwrap_or_default()
                .trim_end_matches('.');
            if target.is_empty() {
                return Ok(Outcome::NxDomain);
            }
            // An alias is resolved from the Service's own namespace; an unknown name is outside.
            host = target.to_ascii_lowercase();
            namespace = service_namespace;
            if lookup(store, &namespace, &host)?.is_none() {
                return Ok(Outcome::External { host });
            }
            continue;
        }
        return through_service(store, source, &service, &service_namespace, port, listens);
    }
    Ok(Outcome::NxDomain)
}

/// The Service a name refers to, with its namespace, following the search path of a pod in
/// `namespace`.
fn lookup(store: &ClusterStore, namespace: &str, host: &str) -> Result<Option<(Value, String)>, StoreError> {
    let search = [
        format!("{namespace}.svc.{CLUSTER_DOMAIN}"),
        format!("svc.{CLUSTER_DOMAIN}"),
        CLUSTER_DOMAIN.to_string(),
    ];
    let mut candidates: Vec<String> = search.iter().map(|s| format!("{host}.{s}")).collect();
    if host.matches('.').count() >= NDOTS {
        candidates.insert(0, host.to_string());
    } else {
        candidates.push(host.to_string());
    }
    for candidate in candidates {
        let Some(rest) = candidate.strip_suffix(&format!(".svc.{CLUSTER_DOMAIN}")) else {
            continue;
        };
        let mut parts = rest.split('.');
        let (Some(name), Some(service_namespace), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let Some(service) = store.get("Service", service_namespace, name)? {
            return Ok(Some((service, service_namespace.to_string())));
        }
    }
    Ok(None)
}

/// A connection to a literal IP: a Service's cluster IP, or a pod's.
fn by_address(
    store: &ClusterStore,
    source: &Value,
    ip: &str,
    port: i64,
    listens: &dyn Fn(&Value, i64) -> bool,
) -> Result<Outcome, StoreError> {
    for service in store.list_all("Service")? {
        if service["spec"]["clusterIP"].as_str() == Some(ip) {
            let namespace = service["metadata"]["namespace"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            return through_service(store, source, &service, &namespace, port, listens);
        }
    }
    for pod in store.list_all("Pod")? {
        if pod["status"]["podIP"].as_str() == Some(ip) {
            return reach(store, source, &pod, port, listens);
        }
    }
    Ok(Outcome::Timeout)
}

fn through_service(
    store: &ClusterStore,
    source: &Value,
    service: &Value,
    namespace: &str,
    port: i64,
    listens: &dyn Fn(&Value, i64) -> bool,
) -> Result<Outcome, StoreError> {
    let ports = service["spec"]["ports"].as_array().cloned().unwrap_or_default();
    let Some(service_port) = ports.iter().find(|p| p["port"].as_i64() == Some(port)) else {
        return Ok(Outcome::Timeout);
    };
    let name = service["metadata"]["name"].as_str().unwrap_or_default();
    let Some(endpoints) = store.get("Endpoints", namespace, name)? else {
        return Ok(Outcome::Refused);
    };
    // Ready endpoints of the subset that serves this Service port, in address order.
    let port_name = service_port["name"].as_str();
    let mut targets = BTreeSet::new();
    for subset in endpoints["subsets"].as_array().into_iter().flatten() {
        for subset_port in subset["ports"].as_array().into_iter().flatten() {
            if subset_port["name"].as_str() != port_name {
                continue;
            }
            for address in subset["addresses"].as_array().into_iter().flatten() {
                let pod = address["targetRef"]["name"].as_str().unwrap_or_default().to_string();
                targets.insert((pod, subset_port["port"].as_i64().unwrap_or(port)));
            }
        }
    }
    // The first ready endpoint answers; a pod that is gone or deaf is a refusal.
    let Some((pod, target_port)) = targets.into_iter().next() else {
        return Ok(Outcome::Refused);
    };
    match store.get("Pod", namespace, &pod)? {
        Some(pod) => reach(store, source, &pod, target_port, listens),
        None => Ok(Outcome::Refused),
    }
}

/// The last hops: the policies of both ends, then whether the pod listens.
fn reach(
    store: &ClusterStore,
    source: &Value,
    pod: &Value,
    port: i64,
    listens: &dyn Fn(&Value, i64) -> bool,
) -> Result<Outcome, StoreError> {
    if !policy::allows(store, source, pod, port)? {
        return Ok(Outcome::Timeout);
    }
    Ok(if listens(pod, port) {
        Outcome::Connected {
            namespace: pod["metadata"]["namespace"].as_str().unwrap_or_default().to_string(),
            pod: pod["metadata"]["name"].as_str().unwrap_or_default().to_string(),
            port,
        }
    } else {
        Outcome::Refused
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn put(store: &ClusterStore, kind: &str, namespace: &str, name: &str, mut manifest: Value) {
        manifest["metadata"] = json!({"name": name, "namespace": namespace});
        store.insert(kind, "v1", namespace, name, &manifest).unwrap();
    }

    /// `db` in `data` listens on 5432 behind Service `postgres`; `ext` is an alias to it.
    fn cluster() -> ClusterStore {
        let store = ClusterStore::new().unwrap();
        put(
            &store,
            "Pod",
            "data",
            "db-1",
            json!({"status": {"podIP": "10.244.0.9"}}),
        );
        put(
            &store,
            "Service",
            "data",
            "postgres",
            json!({"spec": {"clusterIP": "10.96.0.10", "ports": [{"port": 5432, "targetPort": 5432}]}}),
        );
        put(
            &store,
            "Endpoints",
            "data",
            "postgres",
            json!({"subsets": [{"addresses": [{"ip": "10.244.0.9", "targetRef": {"name": "db-1"}}],
                                "ports": [{"port": 5432}]}]}),
        );
        put(
            &store,
            "Service",
            "app",
            "db",
            json!({"spec": {"type": "ExternalName", "externalName": "postgres.data.svc.cluster.local"}}),
        );
        put(
            &store,
            "Service",
            "app",
            "stripe",
            json!({"spec": {"type": "ExternalName", "externalName": "api.stripe.com"}}),
        );
        put(
            &store,
            "Service",
            "data",
            "empty",
            json!({"spec": {"ports": [{"port": 80}]}}),
        );
        store
    }

    fn listens_on_5432(_: &Value, port: i64) -> bool {
        port == 5432
    }

    fn go(store: &ClusterStore, from: &str, host: &str, port: i64) -> Outcome {
        connect(
            store,
            &json!({"metadata": {"namespace": from}}),
            host,
            port,
            &listens_on_5432,
        )
        .unwrap()
    }

    fn connected() -> Outcome {
        Outcome::Connected {
            namespace: "data".into(),
            pod: "db-1".into(),
            port: 5432,
        }
    }

    #[test]
    fn names_resolve_through_the_search_path() {
        let store = cluster();
        assert_eq!(go(&store, "data", "postgres", 5432), connected());
        for host in [
            "postgres.data",
            "postgres.data.svc",
            "postgres.data.svc.cluster.local",
            "postgres.data.svc.cluster.local.",
        ] {
            assert_eq!(go(&store, "app", host, 5432), connected(), "{host}");
        }
        // A short name is looked up in the caller's own namespace only.
        assert_eq!(go(&store, "app", "postgres", 5432), Outcome::NxDomain);
        assert_eq!(go(&store, "app", "ghost.data", 5432), Outcome::NxDomain);
    }

    #[test]
    fn addresses_connect_to_services_and_pods() {
        let store = cluster();
        assert_eq!(go(&store, "app", "10.96.0.10", 5432), connected());
        assert_eq!(go(&store, "app", "10.244.0.9", 5432), connected());
        assert_eq!(go(&store, "app", "10.244.0.9", 80), Outcome::Refused);
        assert_eq!(go(&store, "app", "10.1.2.3", 80), Outcome::Timeout);
    }

    #[test]
    fn external_names_are_followed() {
        let store = cluster();
        assert_eq!(go(&store, "app", "db", 5432), connected());
        assert_eq!(
            go(&store, "app", "stripe", 443),
            Outcome::External {
                host: "api.stripe.com".into()
            }
        );
    }

    #[test]
    fn ports_endpoints_and_listeners_decide_the_failure() {
        let store = cluster();
        // A port the Service does not expose: nobody answers.
        assert_eq!(go(&store, "data", "postgres", 5433), Outcome::Timeout);
        // No endpoints: refused.
        assert_eq!(go(&store, "data", "empty", 80), Outcome::Refused);
        // The pod is behind the Service but does not listen on the target port.
        let deaf = connect(
            &store,
            &json!({"metadata": {"namespace": "data"}}),
            "postgres",
            5432,
            &|_, _| false,
        )
        .unwrap();
        assert_eq!(deaf, Outcome::Refused);
    }

    #[test]
    fn the_same_state_always_gives_the_same_answer() {
        let store = cluster();
        let first = go(&store, "app", "postgres.data", 5432);
        assert!((0..5).all(|_| go(&store, "app", "postgres.data", 5432) == first));
    }
}
