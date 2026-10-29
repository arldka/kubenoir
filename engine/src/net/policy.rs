//! NetworkPolicy, as the oracle applies it to a connection from one pod to another (V10).
//!
//! A pod that no policy selects for a direction accepts everything in that direction. Once one
//! does, only what some rule of some selecting policy allows gets through. The source's egress
//! policies and the destination's ingress policies must both allow the connection. DNS lookups
//! are never blocked here (a default-deny egress does not break name resolution).

use std::net::Ipv4Addr;

use serde_json::Value;

use crate::store::{ClusterStore, StoreError};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ingress,
    Egress,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Direction::Ingress => "Ingress",
            Direction::Egress => "Egress",
        }
    }

    fn rules(self) -> &'static str {
        match self {
            Direction::Ingress => "ingress",
            Direction::Egress => "egress",
        }
    }

    fn peers(self) -> &'static str {
        match self {
            Direction::Ingress => "from",
            Direction::Egress => "to",
        }
    }
}

/// Whether a TCP connection from `source` to `port` of `dest` passes the policies at both ends.
pub fn allows(store: &ClusterStore, source: &Value, dest: &Value, port: i64) -> Result<bool, StoreError> {
    Ok(permitted(store, Direction::Egress, source, dest, port)?
        && permitted(store, Direction::Ingress, dest, source, port)?)
}

/// `pod` is the pod the policies protect; `other` is the pod at the far end. `port` is a port of
/// the destination, which is `pod` for ingress and `other` for egress.
fn permitted(
    store: &ClusterStore,
    direction: Direction,
    pod: &Value,
    other: &Value,
    port: i64,
) -> Result<bool, StoreError> {
    let namespace = pod["metadata"]["namespace"].as_str().unwrap_or("default");
    let mut selected = false;
    for policy in store.list("NetworkPolicy", namespace)? {
        let spec = &policy["spec"];
        if !covers(spec, direction) || !label_selector(&spec["podSelector"], &pod["metadata"]["labels"]) {
            continue;
        }
        selected = true;
        let destination = if direction == Direction::Ingress { pod } else { other };
        for rule in spec[direction.rules()].as_array().into_iter().flatten() {
            if peers_match(store, rule, direction, namespace, other)? && ports_match(rule, destination, port) {
                return Ok(true);
            }
        }
    }
    Ok(!selected)
}

/// Whether the policy is about `direction`: by `policyTypes`, or by default Ingress always and
/// Egress when it has egress rules.
fn covers(spec: &Value, direction: Direction) -> bool {
    match spec["policyTypes"].as_array() {
        Some(types) if !types.is_empty() => types.iter().any(|t| t.as_str() == Some(direction.name())),
        _ => direction == Direction::Ingress || spec.get("egress").is_some(),
    }
}

fn peers_match(
    store: &ClusterStore,
    rule: &Value,
    direction: Direction,
    policy_namespace: &str,
    other: &Value,
) -> Result<bool, StoreError> {
    let peers = match rule[direction.peers()].as_array() {
        Some(peers) if !peers.is_empty() => peers,
        _ => return Ok(true),
    };
    for peer in peers {
        if peer_matches(store, peer, policy_namespace, other)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn peer_matches(store: &ClusterStore, peer: &Value, policy_namespace: &str, other: &Value) -> Result<bool, StoreError> {
    if peer.get("ipBlock").is_some() {
        let ip = other["status"]["podIP"]
            .as_str()
            .and_then(|ip| ip.parse::<Ipv4Addr>().ok());
        return Ok(ip.is_some_and(|ip| in_block(&peer["ipBlock"], ip)));
    }
    let other_namespace = other["metadata"]["namespace"].as_str().unwrap_or("default");
    let namespace_ok = match peer.get("namespaceSelector") {
        Some(selector) => {
            let labels = store
                .get("Namespace", "", other_namespace)?
                .map(|n| n["metadata"]["labels"].clone())
                .unwrap_or(Value::Null);
            let mut labels = labels.as_object().cloned().unwrap_or_default();
            labels
                .entry("kubernetes.io/metadata.name")
                .or_insert_with(|| Value::String(other_namespace.to_string()));
            label_selector(selector, &Value::Object(labels))
        }
        None => other_namespace == policy_namespace,
    };
    let pod_ok = peer
        .get("podSelector")
        .is_none_or(|selector| label_selector(selector, &other["metadata"]["labels"]));
    Ok(namespace_ok && pod_ok)
}

fn in_block(block: &Value, ip: Ipv4Addr) -> bool {
    let contains = |cidr: &str| {
        let (net, bits) = cidr.split_once('/').unwrap_or((cidr, "32"));
        let (Ok(net), Ok(bits)) = (net.parse::<Ipv4Addr>(), bits.parse::<u32>()) else {
            return false;
        };
        let mask = u32::MAX.checked_shl(32 - bits.min(32)).unwrap_or(0);
        u32::from(net) & mask == u32::from(ip) & mask
    };
    block["cidr"].as_str().is_some_and(contains)
        && !block["except"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(contains)
}

/// A rule without `ports` allows every port; otherwise one of its entries must cover `port`
/// (a name is looked up among the container ports of `destination`).
fn ports_match(rule: &Value, destination: &Value, port: i64) -> bool {
    let ports = match rule["ports"].as_array() {
        Some(ports) if !ports.is_empty() => ports,
        _ => return true,
    };
    ports.iter().any(|entry| {
        if entry["protocol"].as_str().unwrap_or("TCP") != "TCP" {
            return false;
        }
        match &entry["port"] {
            Value::Null => true,
            Value::Number(n) => {
                let first = n.as_i64().unwrap_or(-1);
                let last = entry["endPort"].as_i64().unwrap_or(first);
                (first..=last).contains(&port)
            }
            Value::String(name) => destination["spec"]["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|c| c["ports"].as_array().into_iter().flatten())
                .any(|p| p["name"].as_str() == Some(name) && p["containerPort"].as_i64() == Some(port)),
            _ => false,
        }
    })
}

/// A `LabelSelector` against a label map; an empty selector selects everything.
fn label_selector(selector: &Value, labels: &Value) -> bool {
    let label = |key: &str| labels.get(key).and_then(Value::as_str);
    let equal = selector["matchLabels"]
        .as_object()
        .into_iter()
        .flatten()
        .all(|(k, v)| label(k) == v.as_str());
    let expressions = selector["matchExpressions"].as_array().into_iter().flatten().all(|e| {
        let key = e["key"].as_str().unwrap_or_default();
        let values: Vec<&str> = e["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        match e["operator"].as_str() {
            Some("In") => label(key).is_some_and(|l| values.contains(&l)),
            Some("NotIn") => label(key).is_none_or(|l| !values.contains(&l)),
            Some("Exists") => label(key).is_some(),
            Some("DoesNotExist") => label(key).is_none(),
            _ => false,
        }
    });
    equal && expressions
}
