//! Commands that change the cluster: `set env`, `rollout restart`, `delete`.
//!
//! They only edit desired state (and ask for deletions); the simulated controllers do the rest
//! on the reconcile that follows every command. User-facing failures come back as
//! `Ok(kubectl error text)`; `Err` is reserved for a broken store.

use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{EnvVar, Pod};

use crate::api::{self, KindInfo};
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
                    "The Pod \"{}\" is invalid: {}",
                    args.name,
                    crate::api::POD_UPDATE_FORBIDDEN
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

/// The namespaces the API server will not delete.
const IMMORTAL_NAMESPACES: [&str; 3] = ["default", "kube-public", "kube-system"];

/// How a delete by name ended.
pub enum Deleted {
    Gone,
    Missing,
    /// The API server refused; the text is its error line.
    Forbidden(String),
}

/// Delete one object by name, the way `kubectl delete` asks the API server to.
pub fn delete_named(
    env: &Env<'_>,
    info: &KindInfo,
    namespace: &str,
    name: &str,
    immediate: bool,
) -> Result<Deleted, StoreError> {
    if info.kind == "Namespace" && IMMORTAL_NAMESPACES.contains(&name) {
        return Ok(Deleted::Forbidden(format!(
            "Error from server (Forbidden): namespaces \"{name}\" is forbidden: this namespace may not be deleted"
        )));
    }
    let scope = if info.namespaced { namespace } else { "" };
    Ok(if delete_object(env, info.kind, scope, name, immediate)? {
        Deleted::Gone
    } else {
        Deleted::Missing
    })
}

pub fn delete(args: &DeleteArgs, env: &Env<'_>) -> Result<String, StoreError> {
    let Some(info) = api::info(args.kind) else {
        return Ok(format!("error: unknown kind {}", args.kind));
    };
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let mut lines = Vec::new();
    if args.immediate && info.kind == "Pod" {
        lines.push("Warning: Immediate deletion does not wait for confirmation that the running resource has been terminated. The resource may continue to run on the cluster indefinitely.".to_string());
    }

    for name in &args.names {
        lines.push(match delete_named(env, info, namespace, name, args.immediate)? {
            Deleted::Gone => format!("{} \"{name}\" deleted", info.name_prefix()),
            Deleted::Missing => format!("Error from server (NotFound): {} \"{name}\" not found", info.resource()),
            Deleted::Forbidden(message) => message,
        });
    }
    Ok(lines.join("\n"))
}

/// Delete one object the way the API server and garbage collector would: pods get their grace
/// period, and Deployments and ReplicaSets take what they own with them. Returns whether the
/// object existed. A namespace is only marked: the namespace controller empties it and then
/// removes it, so it shows as `Terminating` meanwhile.
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
        "Namespace" => {
            if manifest["metadata"].get("deletionTimestamp").is_none() {
                let mut terminating = manifest;
                terminating["metadata"]["deletionTimestamp"] = serde_json::json!(clock::format(env.now));
                terminating["status"]["phase"] = serde_json::json!("Terminating");
                env.store.update(kind, namespace, name, &terminating)?;
            }
        }
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
        "Job" => {
            let uid = manifest["metadata"]["uid"].as_str().unwrap_or_default().to_string();
            delete_owned_pods(env, namespace, &uid, immediate)?;
            env.store.delete(kind, namespace, name)?;
            env.store.purge_pod_state(namespace, &format!("job/{name}"))?;
        }
        "CronJob" => {
            let uid = manifest["metadata"]["uid"].as_str().unwrap_or_default().to_string();
            for job in env.store.list("Job", namespace)? {
                let owned = job["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|o| o.iter().any(|r| r["uid"] == uid.as_str()));
                if owned {
                    let job_name = job["metadata"]["name"].as_str().unwrap_or_default().to_string();
                    delete_object(env, "Job", namespace, &job_name, immediate)?;
                }
            }
            env.store.delete(kind, namespace, name)?;
            env.store.purge_pod_state(namespace, &format!("cronjob/{name}"))?;
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
