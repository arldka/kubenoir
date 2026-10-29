//! ReplicaSet controller: keeps `spec.replicas` pods alive from the pod template.

use std::cmp::Reverse;

use k8s_openapi::api::apps::v1::{ReplicaSet, ReplicaSetStatus};
use k8s_openapi::api::core::v1::Pod;
use serde_json::{json, Value};

use super::pods::{delete_pod, is_owned_by, is_ready, owner_ref};
use super::{pods, Env};
use crate::clock;
use crate::events::{self, EventType, Involved, Source};
use crate::names;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let all_pods = pods(env.store)?;
    let mut changed = false;
    for manifest in env.store.list_all("ReplicaSet")? {
        changed |= sync(env, serde_json::from_value(manifest)?, &all_pods)?;
    }
    Ok(changed)
}

fn sync(env: &Env<'_>, mut rs: ReplicaSet, all_pods: &[Pod]) -> Result<bool, StoreError> {
    let namespace = rs
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| DEFAULT_NAMESPACE.into());
    let name = rs.metadata.name.clone().unwrap_or_default();
    let uid = rs.metadata.uid.clone().unwrap_or_default();
    let Some(spec) = rs.spec.clone() else { return Ok(false) };
    let Some(template) = spec.template.as_ref() else {
        return Ok(false);
    };
    let desired = usize::try_from(spec.replicas.unwrap_or(1)).unwrap_or(0);

    let active: Vec<&Pod> = all_pods
        .iter()
        .filter(|p| {
            p.metadata.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE) == namespace
                && is_owned_by(&p.metadata, &uid)
                && p.metadata.deletion_timestamp.is_none()
        })
        .collect();

    let involved = Involved {
        kind: "ReplicaSet",
        api_version: "apps/v1",
        namespace: &namespace,
        name: &name,
        uid: &uid,
        field_path: None,
    };
    let source = Source {
        component: "replicaset-controller",
        host: None,
    };

    if active.len() < desired {
        for _ in active.len()..desired {
            let pod_name = create_pod(env, &namespace, &name, &uid, template)?;
            events::record(
                env.store,
                &involved,
                EventType::Normal,
                "SuccessfulCreate",
                &format!("Created pod: {pod_name}"),
                &source,
                env.now,
            )?;
        }
        return Ok(true);
    }
    if active.len() > desired {
        // Remove the least useful pods first: not ready, then newest.
        let mut victims = active.clone();
        victims.sort_by_key(|p| {
            (
                is_ready(p),
                Reverse(p.metadata.creation_timestamp.as_ref().map_or(0, clock::secs)),
                p.metadata.name.clone(),
            )
        });
        for pod in victims.into_iter().take(active.len() - desired) {
            delete_pod(env, pod, false)?;
            let message = format!("Deleted pod: {}", pod.metadata.name.as_deref().unwrap_or_default());
            events::record(
                env.store,
                &involved,
                EventType::Normal,
                "SuccessfulDelete",
                &message,
                &source,
                env.now,
            )?;
        }
        return Ok(true);
    }

    let ready = active.iter().filter(|p| is_ready(p)).count() as i32;
    let status = ReplicaSetStatus {
        replicas: active.len() as i32,
        ready_replicas: Some(ready),
        available_replicas: Some(ready),
        fully_labeled_replicas: Some(active.len() as i32),
        observed_generation: rs.metadata.generation,
        ..Default::default()
    };
    if rs.status.as_ref() == Some(&status) {
        return Ok(false);
    }
    rs.status = Some(status);
    env.store
        .update("ReplicaSet", &namespace, &name, &serde_json::to_value(&rs)?)?;
    Ok(true)
}

/// Create one pod from the template, with a deterministic generated name.
fn create_pod(
    env: &Env<'_>,
    namespace: &str,
    rs_name: &str,
    rs_uid: &str,
    template: &k8s_openapi::api::core::v1::PodTemplateSpec,
) -> Result<String, StoreError> {
    let name = loop {
        let candidate = format!("{rs_name}-{}", names::pod_suffix(env.store.seed(), env.store.next_id()));
        if env.store.get("Pod", namespace, &candidate)?.is_none() {
            break candidate;
        }
    };

    let template_meta = template.metadata.clone().unwrap_or_default();
    let mut metadata = json!({
        "name": name,
        "generateName": format!("{rs_name}-"),
        "namespace": namespace,
        "creationTimestamp": clock::format(env.now),
        "ownerReferences": [owner_ref("apps/v1", "ReplicaSet", rs_name, rs_uid)],
    });
    if let Some(labels) = template_meta.labels {
        metadata["labels"] = json!(labels);
    }
    if let Some(annotations) = template_meta.annotations {
        metadata["annotations"] = json!(annotations);
    }

    // Defaults the API server fills in for every pod.
    let mut spec: Value = serde_json::to_value(template.spec.clone().unwrap_or_default())?;
    for (key, default) in [
        ("restartPolicy", json!("Always")),
        ("dnsPolicy", json!("ClusterFirst")),
        ("serviceAccountName", json!("default")),
        ("terminationGracePeriodSeconds", json!(30)),
    ] {
        spec.as_object_mut().map(|o| o.entry(key).or_insert(default));
    }

    let pod = json!({ "apiVersion": "v1", "kind": "Pod", "metadata": metadata, "spec": spec });
    env.store.insert("Pod", "v1", namespace, &name, &pod)?;
    Ok(name)
}
