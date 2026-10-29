//! `kubectl describe`, following the layout of the real command.

use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{Container, ContainerState, ContainerStatus, EnvVar, Event, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::jiff::tz::TimeZone;
use serde_json::Value;

use crate::clock;
use crate::names;
use crate::parser::{DescribeArgs, ResourceKind};
use crate::render::{event_age, format_table_with_gap};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::{ClusterStore, StoreError};

const NONE: &str = "<none>";

/// User-facing failures come back as `Ok(kubectl error text)`; `Err` is a broken store.
pub fn describe(args: &DescribeArgs, store: &ClusterStore, now: i64) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    match args.resource {
        ResourceKind::Pod => describe_each(args, store, namespace, |manifest, events| {
            Ok(describe_pod(&serde_json::from_value(manifest)?, events, now))
        }),
        ResourceKind::Deployment => describe_each(args, store, namespace, |manifest, events| {
            let deployment: Deployment = serde_json::from_value(manifest)?;
            let owned = owned_replica_sets(store, namespace, deployment.metadata.uid.as_deref().unwrap_or_default())?;
            Ok(describe_deployment(&deployment, &owned, events, now))
        }),
        ResourceKind::Event | ResourceKind::ReplicaSet => {
            Ok(format!("error: describing {} is not supported", args.resource.plural()))
        }
    }
}

fn owned_replica_sets(store: &ClusterStore, namespace: &str, owner_uid: &str) -> Result<Vec<ReplicaSet>, StoreError> {
    let mut owned = Vec::new();
    for manifest in store.list("ReplicaSet", namespace)? {
        let rs: ReplicaSet = serde_json::from_value(manifest)?;
        let is_owned = rs
            .metadata
            .owner_references
            .iter()
            .flatten()
            .any(|o| o.uid == owner_uid);
        if is_owned {
            owned.push(rs);
        }
    }
    Ok(owned)
}

/// Shared plumbing: resolve names (or all objects in the namespace), gather each object's
/// events and render it with `render`; missing names become NotFound lines at the end.
fn describe_each(
    args: &DescribeArgs,
    store: &ClusterStore,
    namespace: &str,
    render: impl Fn(Value, &[Event]) -> Result<String, StoreError>,
) -> Result<String, StoreError> {
    let kind = args.resource.kind();
    let mut manifests = Vec::new();
    let mut errors = Vec::new();
    if args.names.is_empty() {
        manifests = store.list(kind, namespace)?;
        if manifests.is_empty() {
            return Ok(format!("No resources found in {namespace} namespace."));
        }
    } else {
        for name in &args.names {
            match store.get(kind, namespace, name)? {
                Some(m) => manifests.push(m),
                None => errors.push(format!(
                    "Error from server (NotFound): {} \"{name}\" not found",
                    args.resource.plural()
                )),
            }
        }
    }

    let mut sections = Vec::new();
    for manifest in manifests {
        let name = manifest["metadata"]["name"].as_str().unwrap_or_default().to_string();
        let uid = manifest["metadata"]["uid"].as_str().unwrap_or_default().to_string();
        let events: Vec<Event> = store
            .list("Event", namespace)?
            .into_iter()
            .filter_map(|e| serde_json::from_value::<Event>(e).ok())
            .filter(|e| {
                e.involved_object.kind.as_deref() == Some(kind)
                    && e.involved_object.name.as_deref() == Some(name.as_str())
                    && e.involved_object.uid.as_deref().unwrap_or_default() == uid
            })
            .collect();
        sections.push(render(manifest, &events)?);
    }

    let mut out = sections.join("\n\n\n");
    if !errors.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&errors.join("\n"));
    }
    Ok(out)
}

/// `Tue, 27 Oct 2026 17:43:00 +0000`
fn long_time(time: &Time) -> String {
    time.0
        .to_zoned(TimeZone::UTC)
        .strftime("%a, %d %b %Y %H:%M:%S %z")
        .to_string()
}

/// A `key: value` line whose value starts at absolute column `value_col`.
fn kv(indent: usize, key: &str, value: &str, value_col: usize) -> String {
    let used = indent + key.len();
    format!(
        "{}{key}{}{value}",
        " ".repeat(indent),
        " ".repeat(value_col.saturating_sub(used).max(1))
    )
    .trim_end()
    .to_string()
}

fn value_or_none(value: Option<String>) -> String {
    value.filter(|v| !v.is_empty()).unwrap_or_else(|| NONE.to_string())
}

pub fn describe_pod(pod: &Pod, events: &[Event], now: i64) -> String {
    let meta = &pod.metadata;
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();
    let mut out: Vec<String> = Vec::new();

    // Header block: every value starts two columns past the longest key.
    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    rows.push(("Name:".into(), vec![meta.name.clone().unwrap_or_default()]));
    rows.push(("Namespace:".into(), vec![meta.namespace.clone().unwrap_or_default()]));
    rows.push((
        "Priority:".into(),
        vec![spec.and_then(|s| s.priority).unwrap_or(0).to_string()],
    ));
    if let Some(account) = spec.and_then(|s| s.service_account_name.clone()) {
        rows.push(("Service Account:".into(), vec![account]));
    }
    let node = match (
        spec.and_then(|s| s.node_name.clone()),
        status.and_then(|s| s.host_ip.clone()),
    ) {
        (Some(node), Some(ip)) => format!("{node}/{ip}"),
        (Some(node), None) => node,
        (None, _) => NONE.to_string(),
    };
    rows.push(("Node:".into(), vec![node]));
    rows.push((
        "Start Time:".into(),
        vec![status
            .and_then(|s| s.start_time.as_ref())
            .map_or_else(|| "<unknown>".to_string(), long_time)],
    ));
    rows.push(("Labels:".into(), map_lines(meta.labels.as_ref(), "=")));
    rows.push(("Annotations:".into(), map_lines(meta.annotations.as_ref(), ": ")));
    let phase = status.and_then(|s| s.phase.clone()).unwrap_or_default();
    rows.push((
        "Status:".into(),
        vec![match &meta.deletion_timestamp {
            Some(ts) => format!("Terminating (lasts {}s)", (clock::secs(ts) - now).max(0)),
            None => phase,
        }],
    ));
    if let Some(ip) = status.and_then(|s| s.pod_ip.clone()) {
        rows.push(("IP:".into(), vec![ip.clone()]));
        rows.push(("IPs:".into(), vec![]));
        rows.push(("  IP:".into(), vec![ip]));
    }
    if let Some(owner) = meta
        .owner_references
        .iter()
        .flatten()
        .find(|o| o.controller == Some(true))
    {
        rows.push(("Controlled By:".into(), vec![format!("{}/{}", owner.kind, owner.name)]));
    }
    push_block(&mut out, 0, &rows);

    out.push("Containers:".into());
    let statuses = status.and_then(|s| s.container_statuses.as_deref()).unwrap_or_default();
    for container in spec.map(|s| s.containers.as_slice()).unwrap_or_default() {
        let status = statuses.iter().find(|s| s.name == container.name);
        out.push(format!("  {}:", container.name));
        container_lines(&mut out, container, status);
    }

    out.push("Conditions:".into());
    let conditions = status.and_then(|s| s.conditions.as_deref()).unwrap_or_default();
    let rows: Vec<Vec<String>> = conditions
        .iter()
        .map(|c| vec![c.type_.clone(), c.status.clone()])
        .collect();
    out.extend(indented(&format_table_with_gap(&["Type", "Status"], &rows, 3), 2));

    let mut tail: Vec<(String, Vec<String>)> = vec![
        (
            "QoS Class:".into(),
            vec![status
                .and_then(|s| s.qos_class.clone())
                .unwrap_or_else(|| "BestEffort".into())],
        ),
        (
            "Node-Selectors:".into(),
            match spec.and_then(|s| s.node_selector.as_ref()) {
                Some(selector) if !selector.is_empty() => map_lines(Some(selector), "="),
                _ => vec![NONE.into()],
            },
        ),
    ];
    tail.push(("Tolerations:".into(), tolerations(spec)));
    if events.is_empty() {
        tail.push(("Events:".into(), vec![NONE.into()]));
    }
    push_block(&mut out, 0, &tail);

    out.extend(events_section(events, now));

    out.join("\n")
}

/// The `Events:` table of a describe, or nothing when there are no events.
fn events_section(events: &[Event], now: i64) -> Vec<String> {
    if events.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| {
        (
            e.last_timestamp.as_ref().map(clock::secs).unwrap_or(0),
            e.metadata.name.clone(),
        )
    });
    let mut rows = vec![vec![
        "----".to_string(),
        "------".into(),
        "---".into(),
        "----".into(),
        "-------".into(),
    ]];
    rows.extend(sorted.iter().map(|e| {
        vec![
            e.type_.clone().unwrap_or_default(),
            e.reason.clone().unwrap_or_default(),
            event_age(e, now),
            e.source.as_ref().and_then(|s| s.component.clone()).unwrap_or_default(),
            e.message.clone().unwrap_or_default(),
        ]
    }));
    let mut out = vec!["Events:".to_string()];
    out.extend(indented(
        &format_table_with_gap(&["Type", "Reason", "Age", "From", "Message"], &rows, 2),
        2,
    ));
    out
}

/// `k=v` / `k: v` lines sorted by key, or `<none>`.
fn map_lines(map: Option<&std::collections::BTreeMap<String, String>>, sep: &str) -> Vec<String> {
    match map {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| format!("{k}{sep}{v}")).collect(),
        _ => vec![NONE.to_string()],
    }
}

/// The two tolerations the admission controller adds to every pod unless it brings its own.
fn tolerations(spec: Option<&k8s_openapi::api::core::v1::PodSpec>) -> Vec<String> {
    match spec.and_then(|s| s.tolerations.as_ref()) {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|t| {
                let key = t.key.clone().unwrap_or_default();
                let effect = t.effect.clone().unwrap_or_default();
                let op = t.operator.clone().unwrap_or_else(|| "Equal".into());
                let mut line = format!("{key}:{effect} op={op}");
                if let Some(secs) = t.toleration_seconds {
                    line.push_str(&format!(" for {secs}s"));
                }
                line
            })
            .collect(),
        _ => vec![
            "node.kubernetes.io/not-ready:NoExecute op=Exists for 300s".into(),
            "node.kubernetes.io/unreachable:NoExecute op=Exists for 300s".into(),
        ],
    }
}

/// Write `rows` as an aligned block; continuation lines of a value align with the first.
fn push_block(out: &mut Vec<String>, indent: usize, rows: &[(String, Vec<String>)]) {
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0) + 2;
    for (key, values) in rows {
        let first = values.first().map_or("", String::as_str);
        out.push(kv(indent, key, first, indent + width));
        for value in values.iter().skip(1) {
            out.push(format!("{}{value}", " ".repeat(indent + width)));
        }
    }
}

fn indented(text: &str, by: usize) -> Vec<String> {
    text.lines().map(|l| format!("{}{l}", " ".repeat(by))).collect()
}

/// Container details at indent 4. With a `status` this is a live pod container; without one it
/// is a pod template (as shown in `describe deployment`).
fn container_lines(out: &mut Vec<String>, c: &Container, status: Option<&ContainerStatus>) {
    const INDENT: usize = 4;
    // Longest key is "Restart Count:" for live containers and "Host Port:" for templates.
    let value_col = INDENT + if status.is_some() { 16 } else { 12 };

    if let Some(id) = status.and_then(|s| s.container_id.clone()) {
        out.push(kv(INDENT, "Container ID:", &id, value_col));
    }
    out.push(kv(INDENT, "Image:", c.image.as_deref().unwrap_or_default(), value_col));
    if let Some(id) = status.map(|s| s.image_id.clone()).filter(|i| !i.is_empty()) {
        out.push(kv(INDENT, "Image ID:", &id, value_col));
    }
    let ports = c.ports.as_deref().unwrap_or_default();
    let port_list = |f: &dyn Fn(&k8s_openapi::api::core::v1::ContainerPort) -> i32| {
        value_or_none(Some(
            ports
                .iter()
                .map(|p| format!("{}/{}", f(p), p.protocol.as_deref().unwrap_or("TCP")))
                .collect::<Vec<_>>()
                .join(", "),
        ))
    };
    out.push(kv(INDENT, "Port:", &port_list(&|p| p.container_port), value_col));
    out.push(kv(
        INDENT,
        "Host Port:",
        &port_list(&|p| p.host_port.unwrap_or(0)),
        value_col,
    ));

    if let Some(status) = status {
        state_lines(out, "State:", status.state.as_ref(), value_col);
        state_lines(out, "Last State:", status.last_state.as_ref(), value_col);
        out.push(kv(
            INDENT,
            "Ready:",
            if status.ready { "True" } else { "False" },
            value_col,
        ));
        out.push(kv(
            INDENT,
            "Restart Count:",
            &status.restart_count.to_string(),
            value_col,
        ));
    }

    match c.env.as_deref() {
        Some(vars) if !vars.is_empty() => {
            out.push(kv(INDENT, "Environment:", "", value_col));
            let width = vars.iter().map(|v| v.name.len()).max().unwrap_or(0) + 1 + 2;
            for var in vars {
                out.push(kv(
                    INDENT + 2,
                    &format!("{}:", var.name),
                    &env_value(var),
                    INDENT + 2 + width,
                ));
            }
        }
        _ => out.push(kv(INDENT, "Environment:", NONE, value_col)),
    }
    if status.is_none() {
        out.push(kv(INDENT, "Mounts:", NONE, value_col));
    }
}

fn state_lines(out: &mut Vec<String>, title: &str, state: Option<&ContainerState>, value_col: usize) {
    let Some(state) = state else { return };
    let nested = |out: &mut Vec<String>, key: &str, value: &str| out.push(kv(6, key, value, value_col));
    if let Some(w) = &state.waiting {
        out.push(kv(4, title, "Waiting", value_col));
        if let Some(reason) = w.reason.as_deref().filter(|r| !r.is_empty()) {
            nested(out, "Reason:", reason);
        }
    } else if let Some(r) = &state.running {
        out.push(kv(4, title, "Running", value_col));
        if let Some(started) = &r.started_at {
            nested(out, "Started:", &long_time(started));
        }
    } else if let Some(t) = &state.terminated {
        out.push(kv(4, title, "Terminated", value_col));
        if let Some(reason) = t.reason.as_deref().filter(|r| !r.is_empty()) {
            nested(out, "Reason:", reason);
        }
        if let Some(message) = t.message.as_deref().filter(|m| !m.is_empty()) {
            nested(out, "Message:", message);
        }
        nested(out, "Exit Code:", &t.exit_code.to_string());
        if let Some(started) = &t.started_at {
            nested(out, "Started:", &long_time(started));
        }
        if let Some(finished) = &t.finished_at {
            nested(out, "Finished:", &long_time(finished));
        }
    }
}

fn int_or_percent(value: Option<&IntOrString>, default: &str) -> String {
    match value {
        Some(IntOrString::Int(n)) => n.to_string(),
        Some(IntOrString::String(s)) => s.clone(),
        None => default.to_string(),
    }
}

pub fn describe_deployment(deployment: &Deployment, owned: &[ReplicaSet], events: &[Event], now: i64) -> String {
    let meta = &deployment.metadata;
    let spec = deployment.spec.as_ref();
    let status = deployment.status.as_ref();
    let mut out: Vec<String> = Vec::new();

    let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
    let count =
        |f: &dyn Fn(&k8s_openapi::api::apps::v1::DeploymentStatus) -> Option<i32>| status.and_then(f).unwrap_or(0);
    let strategy = spec.and_then(|s| s.strategy.as_ref());
    let strategy_type = strategy
        .and_then(|s| s.type_.clone())
        .unwrap_or_else(|| "RollingUpdate".into());

    let mut rows: Vec<(String, Vec<String>)> = vec![
        ("Name:".into(), vec![meta.name.clone().unwrap_or_default()]),
        ("Namespace:".into(), vec![meta.namespace.clone().unwrap_or_default()]),
        (
            "CreationTimestamp:".into(),
            vec![meta
                .creation_timestamp
                .as_ref()
                .map_or_else(|| "<unknown>".to_string(), long_time)],
        ),
        ("Labels:".into(), map_lines(meta.labels.as_ref(), "=")),
        ("Annotations:".into(), map_lines(meta.annotations.as_ref(), ": ")),
        (
            "Selector:".into(),
            vec![value_or_none(spec.and_then(|s| s.selector.match_labels.as_ref()).map(
                |m| m.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","),
            ))],
        ),
        (
            "Replicas:".into(),
            vec![format!(
                "{desired} desired | {} updated | {} total | {} available | {} unavailable",
                count(&|s| s.updated_replicas),
                count(&|s| s.replicas),
                count(&|s| s.available_replicas),
                count(&|s| s.unavailable_replicas)
            )],
        ),
        ("StrategyType:".into(), vec![strategy_type.clone()]),
        (
            "MinReadySeconds:".into(),
            vec![spec.and_then(|s| s.min_ready_seconds).unwrap_or(0).to_string()],
        ),
    ];
    if strategy_type == "RollingUpdate" {
        let rolling = strategy.and_then(|s| s.rolling_update.as_ref());
        rows.push((
            "RollingUpdateStrategy:".into(),
            vec![format!(
                "{} max unavailable, {} max surge",
                int_or_percent(rolling.and_then(|r| r.max_unavailable.as_ref()), "25%"),
                int_or_percent(rolling.and_then(|r| r.max_surge.as_ref()), "25%")
            )],
        ));
    }
    push_block(&mut out, 0, &rows);

    out.push("Pod Template:".into());
    let template = spec.map(|s| &s.template);
    let template_labels = template
        .and_then(|t| t.metadata.as_ref())
        .and_then(|m| m.labels.as_ref());
    push_block(&mut out, 2, &[("Labels:".into(), map_lines(template_labels, "="))]);
    out.push("  Containers:".into());
    for container in template
        .and_then(|t| t.spec.as_ref())
        .map(|s| s.containers.as_slice())
        .unwrap_or_default()
    {
        out.push(format!("   {}:", container.name));
        container_lines(&mut out, container, None);
    }
    out.push(kv(2, "Volumes:", NONE, 2 + 10 + 4));

    out.push("Conditions:".into());
    let conditions = status.and_then(|s| s.conditions.as_deref()).unwrap_or_default();
    // kubectl leaves the Type column two spaces wider than the other columns here.
    let wide_type = |t: &str| format!("{t}  ");
    let mut rows = vec![vec![wide_type("----"), "------".to_string(), "------".into()]];
    rows.extend(conditions.iter().map(|c| {
        vec![
            wide_type(&c.type_),
            c.status.clone(),
            c.reason.clone().unwrap_or_default(),
        ]
    }));
    let headers = [wide_type("Type"), "Status".to_string(), "Reason".to_string()];
    let header_refs: Vec<&str> = headers.iter().map(String::as_str).collect();
    out.extend(indented(&format_table_with_gap(&header_refs, &rows, 2), 2));

    // The new ReplicaSet is the one stamped with the current template's hash.
    let current_hash = template
        .and_then(|t| serde_json::to_value(t).ok())
        .map(|t| names::template_hash(&t));
    let is_new = |rs: &ReplicaSet| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("pod-template-hash"))
            .map(String::as_str)
            == current_hash.as_deref()
    };
    let summary = |rs: &ReplicaSet| {
        format!(
            "{} ({}/{} replicas created)",
            rs.metadata.name.clone().unwrap_or_default(),
            rs.status.as_ref().map_or(0, |s| s.replicas),
            rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1)
        )
    };
    let old: Vec<String> = owned
        .iter()
        .filter(|rs| !is_new(rs) && rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0) > 0)
        .map(summary)
        .collect();
    let new = owned.iter().find(|rs| is_new(rs)).map(summary);
    let tail = [
        (
            "OldReplicaSets:".to_string(),
            vec![if old.is_empty() {
                NONE.to_string()
            } else {
                old.join(", ")
            }],
        ),
        (
            "NewReplicaSet:".to_string(),
            vec![new.unwrap_or_else(|| NONE.to_string())],
        ),
    ];
    let mut tail = tail.to_vec();
    if events.is_empty() {
        tail.push(("Events:".into(), vec![NONE.into()]));
    }
    push_block(&mut out, 0, &tail);
    out.extend(events_section(events, now));

    out.join("\n")
}

fn env_value(var: &EnvVar) -> String {
    if let Some(value) = &var.value {
        return value.clone();
    }
    let Some(from) = &var.value_from else {
        return String::new();
    };
    if let Some(key) = &from.secret_key_ref {
        format!(
            "<set to the key '{}' in secret '{}'>  Optional: {}",
            key.key,
            key.name,
            key.optional.unwrap_or(false)
        )
    } else if let Some(key) = &from.config_map_key_ref {
        format!(
            "<set to the key '{}' of config map '{}'>  Optional: {}",
            key.key,
            key.name,
            key.optional.unwrap_or(false)
        )
    } else if let Some(field) = &from.field_ref {
        format!(
            "({}:{})",
            field.api_version.as_deref().unwrap_or("v1"),
            field.field_path
        )
    } else if let Some(resource) = &from.resource_field_ref {
        format!(
            "{} (container {})",
            resource.resource,
            resource.container_name.as_deref().unwrap_or_default()
        )
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Engine;

    /// A crash-looping pod from the real simulation (typo'd env var), 47 simulated minutes in.
    fn crashloop_engine() -> Engine {
        let json = r#"{
            "id": "d",
            "images": {"app:1": {"app": "script@0.1.0", "config":
                {"logs": [{"line": "starting"}, {"at": 1, "line": "FATAL DATABASE_URL not set"}], "exit": {"at": 1, "code": 1}}}},
            "cluster": {"now": "2026-10-27T18:30:00Z", "nodes": ["worker-1"], "pods": [{
                "apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": "payments", "namespace": "default", "labels": {"app": "payments", "tier": "backend"}},
                "spec": {"serviceAccountName": "default", "containers": [{
                    "name": "app", "image": "app:1", "ports": [{"containerPort": 8080}],
                    "env": [{"name": "DATABSE_URL", "value": "postgres://db:5432/pay"}, {"name": "MODE", "value": "prod"}]
                }]}
            }]}
        }"#;
        let mut e = Engine::from_scenario(json, crate::apps::AppSet::bundled()).unwrap();
        e.advance(47 * 60);
        e
    }

    fn describe_text() -> String {
        crashloop_engine().eval("kubectl describe pod payments")
    }

    #[test]
    fn header_block_is_aligned_and_complete() {
        let out = describe_text();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Name:             payments");
        assert_eq!(lines[1], "Namespace:        default");
        assert_eq!(lines[2], "Priority:         0");
        assert_eq!(lines[3], "Service Account:  default");
        assert_eq!(lines[4], "Node:             worker-1/192.168.49.2");
        assert!(
            lines[5].starts_with("Start Time:       Tue, 27 Oct 2026 18:30:00 +0000"),
            "{}",
            lines[5]
        );
        assert_eq!(lines[6], "Labels:           app=payments");
        assert_eq!(lines[7], "                  tier=backend");
        assert_eq!(lines[8], "Annotations:      <none>");
        assert_eq!(lines[9], "Status:           Running");
        assert!(lines[10].starts_with("IP:               10.244.0."), "{}", lines[10]);
        assert_eq!(lines[11], "IPs:");
        assert!(lines[12].starts_with("  IP:             10.244.0."), "{}", lines[12]);
    }

    #[test]
    fn container_section_shows_crashloop_details_and_the_env_typo() {
        let out = describe_text();
        assert!(
            out.contains("Containers:\n  app:\n    Container ID:   containerd://"),
            "{out}"
        );
        for expected in [
            "    Image:          app:1",
            "    Port:           8080/TCP",
            "    Host Port:      0/TCP",
            "    State:          Waiting",
            "      Reason:       CrashLoopBackOff",
            "    Last State:     Terminated",
            "      Reason:       Error",
            "      Exit Code:    1",
            "    Ready:          False",
            "    Environment:",
            "      DATABSE_URL:  postgres://db:5432/pay",
            "      MODE:         prod",
        ] {
            assert!(out.lines().any(|l| l == expected), "missing {expected:?} in:\n{out}");
        }
        let restarts = out.lines().find(|l| l.starts_with("    Restart Count:")).unwrap();
        let n: u32 = restarts.rsplit(' ').next().unwrap().parse().unwrap();
        assert!(n >= 5, "{restarts}");
    }

    #[test]
    fn conditions_qos_and_tolerations() {
        let out = describe_text();
        for expected in [
            "Conditions:",
            "  Type                        Status",
            "  PodReadyToStartContainers   True",
            "  Initialized                 True",
            "  Ready                       False",
            "  ContainersReady             False",
            "  PodScheduled                True",
            "QoS Class:       BestEffort",
            "Node-Selectors:  <none>",
            "Tolerations:     node.kubernetes.io/not-ready:NoExecute op=Exists for 300s",
            "                 node.kubernetes.io/unreachable:NoExecute op=Exists for 300s",
        ] {
            assert!(out.lines().any(|l| l == expected), "missing {expected:?} in:\n{out}");
        }
    }

    #[test]
    fn events_table_shows_backoff_with_repeat_count() {
        let out = describe_text();
        let events: Vec<&str> = out.lines().skip_while(|l| *l != "Events:").collect();
        assert!(
            events[1].starts_with("  Type") && events[1].contains("Message"),
            "{events:?}"
        );
        assert!(events[2].starts_with("  ----"), "{events:?}");
        let scheduled = events.iter().find(|l| l.contains("Scheduled")).unwrap();
        assert!(
            scheduled.contains("47m")
                && scheduled.contains("default-scheduler")
                && scheduled.ends_with("Successfully assigned default/payments to worker-1"),
            "{scheduled}"
        );
        let backoff = events.iter().find(|l| l.contains("BackOff")).unwrap();
        assert!(backoff.starts_with("  Warning  BackOff"), "{backoff}");
        assert!(backoff.contains(" (x") && backoff.contains(" over 4"), "{backoff}");
        assert!(backoff.contains("kubelet"), "{backoff}");
    }

    #[test]
    fn describing_a_missing_or_empty_target() {
        let mut e = crashloop_engine();
        assert_eq!(
            e.eval("kubectl describe pod nope"),
            "Error from server (NotFound): pods \"nope\" not found"
        );
        assert_eq!(
            e.eval("kubectl describe pod -n ghost"),
            "No resources found in ghost namespace."
        );
        assert!(e
            .eval("kubectl describe pods")
            .starts_with("Name:             payments"));
        let both = e.eval("kubectl describe pod payments nope");
        assert!(both.starts_with("Name:") && both.ends_with("Error from server (NotFound): pods \"nope\" not found"));
    }

    #[test]
    fn static_pods_and_terminating_pods_describe_too() {
        let json = include_str!("../../scenarios/sample-crashloop/scenario.json");
        let mut e = Engine::from_scenario(json, crate::apps::AppSet::bundled()).unwrap();
        let out = e.eval("kubectl describe pod payments-api-5c7d4f9b8-9hjv2");
        assert!(out.contains("      Reason:       CrashLoopBackOff"), "{out}");
        assert!(out.contains("    Restart Count:  14"), "{out}");
        assert!(out.contains("Events:          <none>"), "{out}");
    }

    #[test]
    fn env_value_variants() {
        let var = |v: serde_json::Value| -> EnvVar { serde_json::from_value(v).unwrap() };
        assert_eq!(env_value(&var(serde_json::json!({"name": "A", "value": "x"}))), "x");
        assert_eq!(env_value(&var(serde_json::json!({"name": "A"}))), "");
        assert_eq!(
            env_value(&var(
                serde_json::json!({"name": "A", "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}})
            )),
            "<set to the key 'k' in secret 's'>  Optional: false"
        );
        assert_eq!(
            env_value(&var(
                serde_json::json!({"name": "A", "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}}})
            )),
            "(v1:metadata.name)"
        );
    }
}
