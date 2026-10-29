//! Assigns new pods to nodes. A pod is "new" while it has no `status.phase`; the scheduler is
//! what gives it one (`Pending`), which is also how the kubelet knows to start simulating it.
//! Pods authored with a status in the scenario are static and never touched.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Pod, PodCondition, PodIP, PodStatus};

use super::{pod_identity, pods, Env};
use crate::clock;
use crate::events::{self, EventType, Involved, Source};
use crate::store::StoreError;

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let all = pods(env.store)?;

    // How many live pods each node already runs, for least-loaded placement.
    let mut load: BTreeMap<String, usize> = BTreeMap::new();
    for pod in &all {
        if pod.metadata.deletion_timestamp.is_none() {
            if let Some(node) = pod.spec.as_ref().and_then(|s| s.node_name.clone()) {
                *load.entry(node).or_default() += 1;
            }
        }
    }

    let mut changed = false;
    for mut pod in all {
        if pod.status.as_ref().and_then(|s| s.phase.as_ref()).is_some() {
            continue;
        }
        schedule(env, &mut pod, &mut load)?;
        changed = true;
    }
    Ok(changed)
}

fn schedule(env: &Env<'_>, pod: &mut Pod, load: &mut BTreeMap<String, usize>) -> Result<(), StoreError> {
    let (ns, name, uid) = pod_identity(pod);
    let now_time = clock::to_time(env.now);
    pod.metadata.namespace.get_or_insert_with(|| ns.clone());
    pod.metadata.creation_timestamp.get_or_insert_with(|| now_time.clone());

    let preset = pod.spec.as_ref().and_then(|s| s.node_name.clone());
    let node = preset.or_else(|| {
        env.config
            .nodes
            .iter()
            .min_by_key(|n| load.get(*n).copied().unwrap_or(0))
            .cloned()
    });
    let involved = Involved {
        kind: "Pod",
        api_version: "v1",
        namespace: &ns,
        name: &name,
        uid: &uid,
        field_path: None,
    };

    let Some(node) = node else {
        pod.status = Some(PodStatus {
            phase: Some("Pending".into()),
            conditions: Some(vec![PodCondition {
                type_: "PodScheduled".into(),
                status: "False".into(),
                reason: Some("Unschedulable".into()),
                message: Some("0/0 nodes are available.".into()),
                last_transition_time: Some(now_time),
                ..Default::default()
            }]),
            qos_class: Some("BestEffort".into()),
            ..Default::default()
        });
        events::record(
            env.store,
            &involved,
            EventType::Warning,
            "FailedScheduling",
            "0/0 nodes are available.",
            &Source {
                component: "default-scheduler",
                host: None,
            },
            env.now,
        )?;
        return write(env, pod, &ns, &name);
    };

    *load.entry(node.clone()).or_default() += 1;
    let node_index = env.config.nodes.iter().position(|n| *n == node).unwrap_or(0);
    let pod_ip = format!("10.244.{node_index}.{}", 2 + env.store.next_id() % 250);
    if let Some(spec) = pod.spec.as_mut() {
        spec.node_name = Some(node.clone());
    }
    pod.status = Some(PodStatus {
        phase: Some("Pending".into()),
        host_ip: Some(format!("192.168.49.{}", node_index + 2)),
        pod_ip: Some(pod_ip.clone()),
        pod_ips: Some(vec![PodIP { ip: pod_ip }]),
        conditions: Some(vec![PodCondition {
            type_: "PodScheduled".into(),
            status: "True".into(),
            last_transition_time: Some(now_time),
            ..Default::default()
        }]),
        qos_class: Some("BestEffort".into()),
        ..Default::default()
    });
    events::record(
        env.store,
        &involved,
        EventType::Normal,
        "Scheduled",
        &format!("Successfully assigned {ns}/{name} to {node}"),
        &Source {
            component: "default-scheduler",
            host: None,
        },
        env.now,
    )?;
    write(env, pod, &ns, &name)
}

fn write(env: &Env<'_>, pod: &Pod, ns: &str, name: &str) -> Result<(), StoreError> {
    env.store.update("Pod", ns, name, &serde_json::to_value(pod)?)
}
