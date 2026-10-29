//! The Service controller: hands out cluster IPs and node ports, and keeps the Endpoints and
//! EndpointSlices of every Service that has a selector in step with the pods that match it.
//!
//! Only pods in the Service's namespace that match its selector, are not terminating and have an
//! IP are listed; of those, only the ones with `Ready=True` are *ready* addresses (V11). Services
//! without a selector are left alone, their Endpoints belong to whoever wrote them.

use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::Pod;
use serde_json::{json, Map, Value};

use super::pods::is_ready;
use super::Env;
use crate::clock;
use crate::names;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::selector::Selector;
use crate::store::StoreError;

const SERVICE_NAME_LABEL: &str = "kubernetes.io/service-name";
const MANAGED_BY_LABEL: &str = "endpointslice.kubernetes.io/managed-by";
const MANAGER: &str = "endpointslice-controller.k8s.io";
const FIRST_NODE_PORT: i64 = 30000;
const LAST_NODE_PORT: i64 = 32767;

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut changed = allocate(env)?;
    let services = env.store.list_all("Service")?;
    let pods = env.store.list_all("Pod")?;
    for service in &services {
        changed |= sync_endpoints(env, service, &pods)?;
    }
    changed |= remove_orphans(env, &services)?;
    Ok(changed)
}

fn namespace_of(object: &Value) -> &str {
    object["metadata"]["namespace"].as_str().unwrap_or(DEFAULT_NAMESPACE)
}

fn name_of(object: &Value) -> &str {
    object["metadata"]["name"].as_str().unwrap_or_default()
}

// ---- cluster IPs and node ports ----

/// Give every Service the cluster IP and node ports the API server would have assigned.
fn allocate(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut services = env.store.list_all("Service")?;
    let mut used_ips: BTreeSet<String> = services
        .iter()
        .filter_map(|s| s["spec"]["clusterIP"].as_str().map(str::to_string))
        .collect();
    let mut used_ports: BTreeSet<i64> = services
        .iter()
        .flat_map(|s| s["spec"]["ports"].as_array().into_iter().flatten())
        .filter_map(|p| p["nodePort"].as_i64())
        .collect();

    let mut changed = false;
    for service in &mut services {
        let before = service.clone();
        let kind = service["spec"]["type"].as_str().unwrap_or("ClusterIP").to_string();
        let spec = &mut service["spec"];
        if kind != "ExternalName" {
            let current = spec["clusterIP"].as_str().unwrap_or_default().to_string();
            let ip = if current.is_empty() {
                let ip = next_cluster_ip(&used_ips);
                used_ips.insert(ip.clone());
                ip
            } else {
                current
            };
            spec["clusterIP"] = json!(ip);
            spec["clusterIPs"] = json!([ip]);
            spec["ipFamilies"] = json!(["IPv4"]);
            spec["ipFamilyPolicy"] = json!("SingleStack");
        }
        if kind == "NodePort" || kind == "LoadBalancer" {
            if let Some(ports) = spec["ports"].as_array_mut() {
                for port in ports {
                    if port["nodePort"].as_i64().is_none_or(|n| n == 0) {
                        let Some(node_port) = (FIRST_NODE_PORT..=LAST_NODE_PORT).find(|n| !used_ports.contains(n))
                        else {
                            continue;
                        };
                        used_ports.insert(node_port);
                        port["nodePort"] = json!(node_port);
                    }
                }
            }
        }
        if *service != before {
            let (namespace, name) = (namespace_of(service).to_string(), name_of(service).to_string());
            env.store.update("Service", &namespace, &name, service)?;
            changed = true;
        }
    }
    Ok(changed)
}

/// The lowest free address of the service range, `10.96.0.0/16`.
fn next_cluster_ip(used: &BTreeSet<String>) -> String {
    (10u32..)
        .map(|n| format!("10.96.{}.{}", n / 256, n % 256))
        .find(|ip| !used.contains(ip) && !ip.ends_with(".0") && !ip.ends_with(".255"))
        .expect("the service range is not exhausted")
}

// ---- endpoints ----

/// One resolved port of a Service as a particular pod serves it.
type PodPort = (Option<String>, i64, String);

struct Address {
    ip: String,
    node: Option<String>,
    target: Value,
    ready: bool,
}

fn selector_of(service: &Value) -> Option<Selector> {
    let map = service["spec"]["selector"].as_object().filter(|m| !m.is_empty())?;
    let text = map
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(",");
    Selector::parse(&text).ok()
}

fn sync_endpoints(env: &Env<'_>, service: &Value, all_pods: &[Value]) -> Result<bool, StoreError> {
    if service["spec"]["type"].as_str() == Some("ExternalName") {
        return Ok(false);
    }
    let Some(selector) = selector_of(service) else {
        return Ok(false);
    };
    let namespace = namespace_of(service);
    let ports = service["spec"]["ports"].as_array().cloned().unwrap_or_default();

    // Group the matching pods by the ports they serve: named target ports differ per pod.
    let mut groups: BTreeMap<Vec<PodPort>, Vec<Address>> = BTreeMap::new();
    for pod in all_pods {
        if namespace_of(pod) != namespace
            || !selector.matches(pod)
            || pod["metadata"].get("deletionTimestamp").is_some()
        {
            continue;
        }
        let phase = pod["status"]["phase"].as_str().unwrap_or_default();
        let Some(ip) = pod["status"]["podIP"]
            .as_str()
            .filter(|_| !matches!(phase, "Succeeded" | "Failed"))
        else {
            continue;
        };
        let Some(resolved) = ports.iter().map(|p| resolve_port(p, pod)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let typed: Pod = serde_json::from_value(pod.clone())?;
        groups.entry(resolved).or_default().push(Address {
            ip: ip.to_string(),
            node: pod["spec"]["nodeName"].as_str().map(str::to_string),
            target: json!({"kind": "Pod", "name": name_of(pod), "namespace": namespace, "uid": pod["metadata"]["uid"]}),
            ready: is_ready(&typed),
        });
    }
    for addresses in groups.values_mut() {
        addresses.sort_by(|a, b| a.ip.cmp(&b.ip));
    }

    let mut changed = write_endpoints(env, service, &groups)?;
    changed |= write_slices(env, service, &groups)?;
    Ok(changed)
}

/// The port a pod serves for one Service port: `targetPort` as a number, or the container port
/// of that name. `None` if the pod does not declare the name.
fn resolve_port(port: &Value, pod: &Value) -> Option<PodPort> {
    let protocol = port["protocol"].as_str().unwrap_or("TCP").to_string();
    let name = port["name"].as_str().map(str::to_string);
    let number = match &port["targetPort"] {
        Value::Number(n) => n.as_i64()?,
        Value::String(wanted) => pod["spec"]["containers"]
            .as_array()?
            .iter()
            .flat_map(|c| c["ports"].as_array().into_iter().flatten())
            .find(|p| p["name"].as_str() == Some(wanted))?["containerPort"]
            .as_i64()?,
        _ => port["port"].as_i64()?,
    };
    Some((name, number, protocol))
}

fn port_json(port: &PodPort) -> Value {
    let mut out = Map::new();
    if let Some(name) = &port.0 {
        out.insert("name".into(), json!(name));
    }
    out.insert("port".into(), json!(port.1));
    out.insert("protocol".into(), json!(port.2));
    Value::Object(out)
}

fn service_labels(service: &Value) -> Map<String, Value> {
    service["metadata"]["labels"].as_object().cloned().unwrap_or_default()
}

/// Insert `desired`, or update `existing` when `same` says the content moved on; metadata that
/// belongs to the stored object (uid, creation time) is kept.
fn store_object(
    env: &Env<'_>,
    kind: &str,
    desired: &mut Value,
    existing: Option<&Value>,
    fields: &[&str],
) -> Result<bool, StoreError> {
    let namespace = namespace_of(desired).to_string();
    let name = name_of(desired).to_string();
    let api_version = desired["apiVersion"].as_str().unwrap_or("v1").to_string();
    let Some(existing) = existing else {
        env.store.insert(kind, &api_version, &namespace, &name, desired)?;
        return Ok(true);
    };
    let same = fields.iter().all(|f| desired.get(f) == existing.get(f))
        && desired["metadata"]["labels"] == existing["metadata"]["labels"];
    if same {
        return Ok(false);
    }
    for key in ["uid", "creationTimestamp"] {
        desired["metadata"][key] = existing["metadata"][key].clone();
    }
    env.store.update(kind, &namespace, &name, desired)?;
    Ok(true)
}

fn write_endpoints(
    env: &Env<'_>,
    service: &Value,
    groups: &BTreeMap<Vec<PodPort>, Vec<Address>>,
) -> Result<bool, StoreError> {
    let (namespace, name) = (namespace_of(service), name_of(service));
    let address = |a: &Address| {
        let mut out = json!({"ip": a.ip, "targetRef": a.target});
        if let Some(node) = &a.node {
            out["nodeName"] = json!(node);
        }
        out
    };
    let subsets: Vec<Value> = groups
        .iter()
        .map(|(ports, addresses)| {
            let mut subset = json!({"ports": ports.iter().map(port_json).collect::<Vec<_>>()});
            let ready: Vec<Value> = addresses.iter().filter(|a| a.ready).map(address).collect();
            let waiting: Vec<Value> = addresses.iter().filter(|a| !a.ready).map(address).collect();
            if !ready.is_empty() {
                subset["addresses"] = json!(ready);
            }
            if !waiting.is_empty() {
                subset["notReadyAddresses"] = json!(waiting);
            }
            subset
        })
        .collect();

    let mut desired = json!({
        "apiVersion": "v1",
        "kind": "Endpoints",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "creationTimestamp": clock::format(env.now),
            "labels": service_labels(service),
        },
    });
    if !subsets.is_empty() {
        desired["subsets"] = json!(subsets);
    }
    let existing = env.store.get("Endpoints", namespace, name)?;
    store_object(env, "Endpoints", &mut desired, existing.as_ref(), &["subsets"])
}

fn slice_name(service: &Value, index: usize) -> String {
    let key = format!("{}-{index}", service["metadata"]["uid"].as_str().unwrap_or_default());
    let suffix = names::pod_suffix(0, names::fnv1a(key.as_bytes()));
    format!("{}-{suffix}", name_of(service))
}

fn write_slices(
    env: &Env<'_>,
    service: &Value,
    groups: &BTreeMap<Vec<PodPort>, Vec<Address>>,
) -> Result<bool, StoreError> {
    let (namespace, name) = (namespace_of(service), name_of(service));
    let uid = service["metadata"]["uid"].as_str().unwrap_or_default();

    // A Service nobody backs still gets one empty slice that lists its ports.
    let placeholder;
    let wanted: Vec<(&[PodPort], &[Address])> = if groups.is_empty() {
        placeholder = service["spec"]["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                (
                    p["name"].as_str().map(str::to_string),
                    p["targetPort"].as_i64().unwrap_or(0),
                    p["protocol"].as_str().unwrap_or("TCP").to_string(),
                )
            })
            .collect::<Vec<PodPort>>();
        vec![(placeholder.as_slice(), &[])]
    } else {
        groups.iter().map(|(p, a)| (p.as_slice(), a.as_slice())).collect()
    };

    let mut labels = service_labels(service);
    labels.insert(SERVICE_NAME_LABEL.into(), json!(name));
    labels.insert(MANAGED_BY_LABEL.into(), json!(MANAGER));

    let mut changed = false;
    let mut keep = BTreeSet::new();
    for (index, (ports, addresses)) in wanted.into_iter().enumerate() {
        let slice = slice_name(service, index);
        keep.insert(slice.clone());
        let endpoints: Vec<Value> = addresses
            .iter()
            .map(|a| {
                let mut endpoint = json!({
                    "addresses": [a.ip],
                    "conditions": {"ready": a.ready, "serving": a.ready, "terminating": false},
                    "targetRef": a.target,
                });
                if let Some(node) = &a.node {
                    endpoint["nodeName"] = json!(node);
                }
                endpoint
            })
            .collect();
        let ports: Vec<Value> = ports
            .iter()
            .map(|p| {
                let mut port = port_json(p);
                if p.1 == 0 {
                    port.as_object_mut().map(|o| o.remove("port"));
                }
                port
            })
            .collect();
        let mut desired = json!({
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {
                "name": slice,
                "generateName": format!("{name}-"),
                "namespace": namespace,
                "creationTimestamp": clock::format(env.now),
                "labels": labels,
                "ownerReferences": [super::pods::owner_ref("v1", "Service", name, uid)],
            },
            "addressType": "IPv4",
            "endpoints": endpoints,
            "ports": ports,
        });
        let existing = env.store.get("EndpointSlice", namespace, &slice)?;
        changed |= store_object(
            env,
            "EndpointSlice",
            &mut desired,
            existing.as_ref(),
            &["endpoints", "ports", "addressType"],
        )?;
    }

    // Slices this controller made for the Service that no longer match any port set.
    for stale in env.store.list("EndpointSlice", namespace)? {
        let ours = stale["metadata"]["labels"][SERVICE_NAME_LABEL].as_str() == Some(name)
            && stale["metadata"]["labels"][MANAGED_BY_LABEL].as_str() == Some(MANAGER);
        if ours && !keep.contains(name_of(&stale)) {
            changed |= env.store.delete("EndpointSlice", namespace, name_of(&stale))?;
        }
    }
    Ok(changed)
}

/// Endpoints and slices outlive nothing: when their Service is gone, they go too.
fn remove_orphans(env: &Env<'_>, services: &[Value]) -> Result<bool, StoreError> {
    let alive: BTreeSet<(&str, &str)> = services.iter().map(|s| (namespace_of(s), name_of(s))).collect();
    let mut changed = false;
    for endpoints in env.store.list_all("Endpoints")? {
        if !alive.contains(&(namespace_of(&endpoints), name_of(&endpoints))) {
            changed |= env
                .store
                .delete("Endpoints", namespace_of(&endpoints), name_of(&endpoints))?;
        }
    }
    for slice in env.store.list_all("EndpointSlice")? {
        let labels = &slice["metadata"]["labels"];
        let Some(service) = labels[SERVICE_NAME_LABEL].as_str() else {
            continue;
        };
        if labels[MANAGED_BY_LABEL].as_str() == Some(MANAGER) && !alive.contains(&(namespace_of(&slice), service)) {
            changed |= env
                .store
                .delete("EndpointSlice", namespace_of(&slice), name_of(&slice))?;
        }
    }
    Ok(changed)
}
