//! Commands that change the cluster: `set env`, `rollout restart`, `delete`.
//!
//! They only edit desired state (and ask for deletions); the simulated controllers do the rest
//! on the reconcile that follows every command. User-facing failures come back as
//! `Ok(kubectl error text)`; `Err` is reserved for a broken store.

use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{EnvVar, Pod};

use crate::clock;
use crate::controllers::{delete_pod, is_owned_by, Env};
use crate::parser::{DeleteArgs, EnvChange, ResourceKind, RolloutRestartArgs, SetEnvArgs};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

const RESTARTED_AT: &str = "kubectl.kubernetes.io/restartedAt";

fn not_found(resource: ResourceKind, name: &str) -> String {
    format!(
        "Error from server (NotFound): {} \"{name}\" not found",
        resource.plural()
    )
}

pub fn set_env(args: &SetEnvArgs, env: &Env<'_>) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    match args.resource {
        ResourceKind::Deployment => {}
        ResourceKind::Pod => {
            // Pods are immutable apart from a few fields; the fix belongs on the owning Deployment.
            return Ok(if env.store.get("Pod", namespace, &args.name)?.is_some() {
                format!(
                    "The Pod \"{}\" is invalid: spec: Forbidden: pod updates may not change fields other than `spec.containers[*].image`, `spec.initContainers[*].image`, `spec.activeDeadlineSeconds`, `spec.tolerations` (only additions to existing tolerations), `spec.terminationGracePeriodSeconds` (allow it to be set to 1 if it was previously negative)",
                    args.name
                )
            } else {
                not_found(args.resource, &args.name)
            });
        }
        other => return Ok(format!("error: setting env on {} is not supported", other.plural())),
    }

    let Some(manifest) = env.store.get("Deployment", namespace, &args.name)? else {
        return Ok(not_found(args.resource, &args.name));
    };
    let mut deployment: Deployment = serde_json::from_value(manifest)?;
    let Some(pod_spec) = deployment.spec.as_mut().and_then(|s| s.template.spec.as_mut()) else {
        return Ok(format!("error: deployment \"{}\" has no pod template", args.name));
    };

    let selected: Vec<usize> = (0..pod_spec.containers.len())
        .filter(|i| {
            args.container
                .as_deref()
                .is_none_or(|c| c == "*" || c == pod_spec.containers[*i].name)
        })
        .collect();
    if selected.is_empty() {
        return Ok(format!(
            "error: unable to find container named \"{}\"",
            args.container.as_deref().unwrap_or_default()
        ));
    }

    if args.list {
        let blocks: Vec<String> = selected
            .iter()
            .map(|&i| {
                let container = &pod_spec.containers[i];
                let mut lines = vec![format!("# deployments/{}, container {}", args.name, container.name)];
                lines.extend(
                    container
                        .env
                        .iter()
                        .flatten()
                        .map(|v| format!("{}={}", v.name, v.value.as_deref().unwrap_or_default())),
                );
                lines.join("\n")
            })
            .collect();
        return Ok(blocks.join("\n"));
    }

    let template_before = serde_json::to_value(deployment.spec.as_ref().map(|s| &s.template))?;
    let pod_spec = deployment
        .spec
        .as_mut()
        .and_then(|s| s.template.spec.as_mut())
        .expect("checked above");
    for &i in &selected {
        let vars = pod_spec.containers[i].env.get_or_insert_with(Vec::new);
        for change in &args.changes {
            match change {
                EnvChange::Set { name, value } => {
                    let var = EnvVar {
                        name: name.clone(),
                        value: Some(value.clone()),
                        value_from: None,
                    };
                    match vars.iter_mut().find(|v| v.name == *name) {
                        Some(existing) => *existing = var,
                        None => vars.push(var),
                    }
                }
                EnvChange::Unset(name) => vars.retain(|v| v.name != *name),
            }
        }
        if vars.is_empty() {
            pod_spec.containers[i].env = None;
        }
    }

    // An unchanged template is not a new revision: don't bump the generation for it.
    let template_after = serde_json::to_value(deployment.spec.as_ref().map(|s| &s.template))?;
    if template_after != template_before {
        deployment.metadata.generation = Some(deployment.metadata.generation.unwrap_or(1) + 1);
        env.store
            .update("Deployment", namespace, &args.name, &serde_json::to_value(&deployment)?)?;
    }
    Ok(format!("deployment.apps/{} env updated", args.name))
}

pub fn rollout_restart(args: &RolloutRestartArgs, env: &Env<'_>) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let mut lines = Vec::new();
    for name in &args.names {
        if args.resource != ResourceKind::Deployment {
            lines.push(format!(
                "error: {} \"{name}\" cannot be restarted",
                args.resource.plural()
            ));
            continue;
        }
        let Some(manifest) = env.store.get("Deployment", namespace, name)? else {
            lines.push(not_found(args.resource, name));
            continue;
        };
        let mut deployment: Deployment = serde_json::from_value(manifest)?;
        if let Some(spec) = deployment.spec.as_mut() {
            let meta = spec.template.metadata.get_or_insert_with(Default::default);
            meta.annotations
                .get_or_insert_with(Default::default)
                .insert(RESTARTED_AT.into(), clock::format(env.now));
        }
        deployment.metadata.generation = Some(deployment.metadata.generation.unwrap_or(1) + 1);
        env.store
            .update("Deployment", namespace, name, &serde_json::to_value(&deployment)?)?;
        lines.push(format!("deployment.apps/{name} restarted"));
    }
    Ok(lines.join("\n"))
}

pub fn delete(args: &DeleteArgs, env: &Env<'_>) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let kind = args.resource.kind();
    let mut lines = Vec::new();
    if args.immediate && args.resource == ResourceKind::Pod {
        lines.push("Warning: Immediate deletion does not wait for confirmation that the running resource has been terminated. The resource may continue to run on the cluster indefinitely.".to_string());
    }

    for name in &args.names {
        if delete_object(env, kind, namespace, name, args.immediate)? {
            lines.push(format!("{} \"{name}\" deleted", args.resource.name_prefix()));
        } else {
            lines.push(not_found(args.resource, name));
        }
    }
    Ok(lines.join("\n"))
}

/// Delete one object the way the API server and garbage collector would: pods get their grace
/// period, and Deployments and ReplicaSets take what they own with them. Returns whether the
/// object existed. Callers handle kinds that cannot be deleted here (namespaces).
pub fn delete_object(
    env: &Env<'_>,
    kind: &str,
    namespace: &str,
    name: &str,
    immediate: bool,
) -> Result<bool, StoreError> {
    let Some(manifest) = env.store.get(kind, namespace, name)? else {
        return Ok(false);
    };
    match kind {
        "Pod" => delete_pod(env, &serde_json::from_value::<Pod>(manifest)?, immediate)?,
        "ReplicaSet" => {
            let rs: ReplicaSet = serde_json::from_value(manifest)?;
            delete_owned_pods(
                env,
                namespace,
                rs.metadata.uid.as_deref().unwrap_or_default(),
                immediate,
            )?;
            env.store.delete(kind, namespace, name)?;
        }
        "Deployment" => {
            // Cascade like the garbage collector: Deployment -> ReplicaSets -> pods.
            let deployment: Deployment = serde_json::from_value(manifest)?;
            let uid = deployment.metadata.uid.as_deref().unwrap_or_default();
            for rs_manifest in env.store.list("ReplicaSet", namespace)? {
                let rs: ReplicaSet = serde_json::from_value(rs_manifest)?;
                if is_owned_by(&rs.metadata, uid) {
                    let rs_name = rs.metadata.name.clone().unwrap_or_default();
                    delete_owned_pods(
                        env,
                        namespace,
                        rs.metadata.uid.as_deref().unwrap_or_default(),
                        immediate,
                    )?;
                    env.store.delete("ReplicaSet", namespace, &rs_name)?;
                }
            }
            env.store.delete(kind, namespace, name)?;
        }
        _ => {
            env.store.delete(kind, namespace, name)?;
        }
    }
    Ok(true)
}

fn delete_owned_pods(env: &Env<'_>, namespace: &str, owner_uid: &str, immediate: bool) -> Result<(), StoreError> {
    for manifest in env.store.list("Pod", namespace)? {
        let pod: Pod = serde_json::from_value(manifest)?;
        if is_owned_by(&pod.metadata, owner_uid) {
            delete_pod(env, &pod, immediate)?;
        }
    }
    Ok(())
}
