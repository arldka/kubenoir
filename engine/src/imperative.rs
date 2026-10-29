//! The imperative kubectl commands: `run`, `create KIND`, `set image`, `scale`, `rollout
//! status|history|undo`, `label` and `expose`. Each builds or edits a manifest and sends it
//! through the API server's write path (`crate::api`), so it is validated, defaulted and
//! guarded exactly like `kubectl apply` is.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::api::{self, ApiError, KindInfo, Origin};
use crate::controllers::Env;
use crate::manifests::{normalize_path, Files};
use crate::parser::{
    CreateArgs, CreateObject, ExposeArgs, LabelArgs, LabelChange, RolloutAction, RolloutArgs, RunArgs, ScaleArgs,
    SetImageArgs,
};
use crate::render::format_table_with_gap;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::shell::base64_encode;
use crate::store::StoreError;

const REVISION: &str = "deployment.kubernetes.io/revision";
const CHANGE_CAUSE: &str = "kubernetes.io/change-cause";
const HASH_LABEL: &str = "pod-template-hash";

type Reply = Result<String, StoreError>;

/// How a refused write is shown: validation failures as the bare message, like kubectl's
/// imperative commands; anything else as the server's error line.
fn failed(error: ApiError) -> Reply {
    match error {
        ApiError::Store(e) => Err(e),
        ApiError::Invalid { .. } => Ok(error.message()),
        other => Ok(other.to_string()),
    }
}

fn not_found(info: &KindInfo, name: &str) -> String {
    ApiError::NotFound {
        resource: info.resource(),
        name: name.to_string(),
    }
    .to_string()
}

fn namespace_of(namespace: &Option<String>) -> &str {
    namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE)
}

/// Where an object of this kind is stored: its namespace, or none for cluster-scoped kinds.
fn scope<'a>(info: &KindInfo, namespace: &'a str) -> &'a str {
    if info.namespaced {
        namespace
    } else {
        ""
    }
}

fn create_manifest(env: &Env<'_>, manifest: &Value, namespace: &str) -> Reply {
    match api::create(env.store, manifest, namespace, env.now, Origin::Client) {
        Ok(stored) => Ok(format!("{}/{} created", stored.info.name_prefix(), stored.name)),
        Err(e) => failed(e),
    }
}

// ---- run ------------------------------------------------------------------------------------

pub fn run(args: &RunArgs, env: &Env<'_>) -> Reply {
    let mut container = json!({"name": args.name, "image": args.image});
    if !args.env.is_empty() {
        container["env"] = args.env.iter().map(|(k, v)| json!({"name": k, "value": v})).collect();
    }
    if let Some(port) = args.port {
        container["ports"] = json!([{"containerPort": port}]);
    }
    if !args.trailing.is_empty() {
        container[if args.command { "command" } else { "args" }] = json!(args.trailing);
    }
    let labels: Map<String, Value> = if args.labels.is_empty() {
        Map::from_iter([("run".to_string(), json!(args.name))])
    } else {
        args.labels.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
    };
    let mut spec = json!({"containers": [container]});
    if let Some(policy) = &args.restart {
        spec["restartPolicy"] = json!(policy);
    }
    let pod =
        json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": args.name, "labels": labels}, "spec": spec});
    create_manifest(env, &pod, namespace_of(&args.namespace))
}

// ---- create ---------------------------------------------------------------------------------

/// A container is named after its image: `registry/team/app:1.2` → `app`.
fn image_base(image: &str) -> String {
    let without_digest = image.split('@').next().unwrap_or(image);
    let last = without_digest.rsplit('/').next().unwrap_or(without_digest);
    last.split(':').next().unwrap_or(last).to_string()
}

/// The `data` of a ConfigMap or Secret from `--from-literal` and `--from-file`. `Err` is the
/// message for `error: ...` text.
fn collect_data(
    kind: &str,
    name: &str,
    literals: &[(String, String)],
    sources: &[(Option<String>, String)],
    files: &Files,
    encode: bool,
) -> Result<Map<String, Value>, String> {
    let mut data = Map::new();
    let mut add = |key: String, value: &str| -> Result<(), String> {
        let value = if encode {
            base64_encode(value.as_bytes())
        } else {
            value.to_string()
        };
        if data.insert(key.clone(), json!(value)).is_some() {
            return Err(format!(
                "error: cannot add key \"{key}\", another key by that name already exists in Data for {kind} \"{name}\""
            ));
        }
        Ok(())
    };
    for (key, value) in literals {
        add(key.clone(), value)?;
    }
    for (key, path) in sources {
        let normalized = normalize_path(path);
        let Some(text) = files.get(&normalized) else {
            let is_dir = files.keys().any(|f| f.starts_with(&format!("{normalized}/")));
            let why = if is_dir {
                "is a directory"
            } else {
                "no such file or directory"
            };
            return Err(format!("error: error reading {path}: {why}"));
        };
        let key = key
            .clone()
            .unwrap_or_else(|| normalized.rsplit('/').next().unwrap_or(&normalized).to_string());
        add(key, text)?;
    }
    Ok(data)
}

pub fn create_object(args: &CreateArgs, env: &Env<'_>, files: &Files) -> Reply {
    let name = &args.name;
    let manifest = match &args.object {
        CreateObject::Namespace => json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name}}),
        CreateObject::Deployment { image, replicas, port } => {
            let mut container = json!({"name": image_base(image), "image": image});
            if let Some(port) = port {
                container["ports"] = json!([{"containerPort": port}]);
            }
            let labels = json!({"app": name});
            json!({"apiVersion": "apps/v1", "kind": "Deployment",
                   "metadata": {"name": name, "labels": labels},
                   "spec": {"replicas": replicas, "selector": {"matchLabels": labels},
                            "template": {"metadata": {"labels": labels}, "spec": {"containers": [container]}}}})
        }
        CreateObject::ConfigMap {
            literals,
            files: sources,
        } => match collect_data("ConfigMap", name, literals, sources, files, false) {
            Ok(data) => json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": name}, "data": data}),
            Err(message) => return Ok(message),
        },
        CreateObject::Secret {
            literals,
            files: sources,
            secret_type,
        } => match collect_data("Secret", name, literals, sources, files, true) {
            Ok(data) => {
                let mut secret =
                    json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": name}, "data": data});
                if let Some(secret_type) = secret_type {
                    secret["type"] = json!(secret_type);
                }
                secret
            }
            Err(message) => return Ok(message),
        },
        CreateObject::DockerRegistrySecret {
            server,
            username,
            password,
            email,
        } => {
            let mut entry = json!({"username": username, "password": password,
                                   "auth": base64_encode(format!("{username}:{password}").as_bytes())});
            if let Some(email) = email {
                entry["email"] = json!(email);
            }
            let config = json!({"auths": {server: entry}}).to_string();
            json!({"apiVersion": "v1", "kind": "Secret", "type": "kubernetes.io/dockerconfigjson",
                   "metadata": {"name": name}, "data": {".dockerconfigjson": base64_encode(config.as_bytes())}})
        }
    };
    create_manifest(env, &manifest, namespace_of(&args.namespace))
}

// ---- set image ------------------------------------------------------------------------------

pub fn set_image(args: &SetImageArgs, env: &Env<'_>) -> Reply {
    let Some(info) = api::info(args.kind) else {
        return Ok(format!("error: unknown kind {}", args.kind));
    };
    let namespace = namespace_of(&args.namespace);
    let Some(mut manifest) = env.store.get(info.kind, scope(info, namespace), &args.name)? else {
        return Ok(not_found(info, &args.name));
    };
    let pod_spec = if info.kind == "Pod" {
        "/spec"
    } else {
        "/spec/template/spec"
    };
    for (container, image) in &args.images {
        let mut matched = false;
        for list in ["containers", "initContainers"] {
            let Some(containers) = manifest
                .pointer_mut(&format!("{pod_spec}/{list}"))
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            for c in containers {
                if container == "*" || c["name"] == json!(container) {
                    c["image"] = json!(image);
                    matched = true;
                }
            }
        }
        if !matched {
            return Ok(format!("error: unable to find container named \"{container}\""));
        }
    }
    match api::update(env.store, &manifest, namespace) {
        Ok(_) => Ok(format!("{}/{} image updated", info.name_prefix(), args.name)),
        Err(e) => failed(e),
    }
}

// ---- scale ----------------------------------------------------------------------------------

pub fn scale(args: &ScaleArgs, env: &Env<'_>) -> Reply {
    let Some(info) = api::info(args.kind) else {
        return Ok(format!("error: unknown kind {}", args.kind));
    };
    let namespace = namespace_of(&args.namespace);
    let mut lines = Vec::new();
    for name in &args.names {
        let Some(mut manifest) = env.store.get(info.kind, scope(info, namespace), name)? else {
            lines.push(not_found(info, name));
            continue;
        };
        manifest["spec"]["replicas"] = json!(args.replicas);
        match api::update(env.store, &manifest, namespace) {
            Ok(_) => lines.push(format!("{}/{name} scaled", info.name_prefix())),
            Err(e) => lines.push(failed(e)?),
        }
    }
    Ok(lines.join("\n"))
}

// ---- rollout --------------------------------------------------------------------------------

fn revision_of(meta: &Value) -> i64 {
    meta["metadata"]["annotations"][REVISION]
        .as_str()
        .and_then(|r| r.parse().ok())
        .unwrap_or(0)
}

/// The ReplicaSets a Deployment owns, oldest revision first.
fn history(env: &Env<'_>, namespace: &str, deployment: &Value) -> Result<Vec<(i64, Value)>, StoreError> {
    let uid = deployment["metadata"]["uid"].as_str().unwrap_or_default();
    let mut owned: Vec<(i64, Value)> = env
        .store
        .list("ReplicaSet", namespace)?
        .into_iter()
        .filter(|rs| {
            rs["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|o| o["uid"] == json!(uid)))
        })
        .map(|rs| (revision_of(&rs), rs))
        .collect();
    owned.sort_by_key(|(revision, _)| *revision);
    Ok(owned)
}

fn rollout_status(name: &str, deployment: &Value) -> String {
    let status = &deployment["status"];
    let int = |v: &Value| v.as_i64().unwrap_or(0);
    let desired = deployment["spec"]["replicas"].as_i64().unwrap_or(1);
    let (replicas, updated, available) = (
        int(&status["replicas"]),
        int(&status["updatedReplicas"]),
        int(&status["availableReplicas"]),
    );
    let waiting = |what: String| format!("Waiting for deployment \"{name}\" rollout to finish: {what}...");
    if int(&deployment["metadata"]["generation"]) > int(&status["observedGeneration"]) {
        return "Waiting for deployment spec update to be observed...".into();
    }
    let timed_out = status["conditions"].as_array().is_some_and(|cs| {
        cs.iter()
            .any(|c| c["type"] == "Progressing" && c["reason"] == "ProgressDeadlineExceeded")
    });
    if timed_out {
        format!("error: deployment \"{name}\" exceeded its progress deadline")
    } else if updated < desired {
        waiting(format!("{updated} out of {desired} new replicas have been updated"))
    } else if replicas > updated {
        waiting(format!("{} old replicas are pending termination", replicas - updated))
    } else if available < updated {
        waiting(format!("{available} of {updated} updated replicas are available"))
    } else {
        format!("deployment \"{name}\" successfully rolled out")
    }
}

/// A pod template as a ReplicaSet recorded it, without the label the controller added.
fn template_of(rs: &Value) -> Value {
    let mut template = rs["spec"]["template"].clone();
    if let Some(labels) = template["metadata"]["labels"].as_object_mut() {
        labels.remove(HASH_LABEL);
    }
    template
}

pub fn rollout(args: &RolloutArgs, env: &Env<'_>) -> Reply {
    let Some(info) = api::info("Deployment") else {
        return Ok("error: deployments are not available".into());
    };
    let namespace = namespace_of(&args.namespace);
    let name = &args.name;
    let Some(deployment) = env.store.get(info.kind, namespace, name)? else {
        return Ok(not_found(info, name));
    };
    match &args.action {
        RolloutAction::Status => Ok(rollout_status(name, &deployment)),
        RolloutAction::History => {
            let revisions = history(env, namespace, &deployment)?;
            if revisions.is_empty() {
                return Ok("No rollout history found.".into());
            }
            let rows: Vec<Vec<String>> = revisions
                .iter()
                .map(|(revision, rs)| {
                    let cause = rs["metadata"]["annotations"][CHANGE_CAUSE].as_str().unwrap_or("<none>");
                    vec![revision.to_string(), cause.to_string()]
                })
                .collect();
            Ok(format!(
                "{}/{name} \n{}\n",
                info.name_prefix(),
                format_table_with_gap(&["REVISION", "CHANGE-CAUSE"], &rows, 2)
            ))
        }
        RolloutAction::Undo { to_revision } => {
            let revisions = history(env, namespace, &deployment)?;
            let current = revision_of(&deployment);
            let target = match to_revision {
                Some(wanted) => match revisions.iter().find(|(r, _)| r == wanted) {
                    Some(found) => found,
                    None => return Ok(format!("error: unable to find specified revision {wanted} in history")),
                },
                None => match revisions.iter().rev().find(|(r, _)| *r < current) {
                    Some(found) => found,
                    None => return Ok("error: unable to find last revision".into()),
                },
            };
            let template = template_of(&target.1);
            let mut updated = deployment.clone();
            let mut live_template = updated["spec"]["template"].clone();
            if let Some(labels) = live_template["metadata"]["labels"].as_object_mut() {
                labels.remove(HASH_LABEL);
            }
            if live_template == template {
                return Ok(format!(
                    "{}/{name} skipped rollback (current template already matches revision {})",
                    info.name_prefix(),
                    target.0
                ));
            }
            updated["spec"]["template"] = template;
            match api::update(env.store, &updated, namespace) {
                Ok(_) => Ok(format!("{}/{name} rolled back", info.name_prefix())),
                Err(e) => failed(e),
            }
        }
    }
}

// ---- label ----------------------------------------------------------------------------------

pub fn label(args: &LabelArgs, env: &Env<'_>) -> Reply {
    let Some(info) = api::info(args.kind) else {
        return Ok(format!("error: unknown kind {}", args.kind));
    };
    let namespace = namespace_of(&args.namespace);
    let mut lines = Vec::new();
    for name in &args.names {
        let Some(mut manifest) = env.store.get(info.kind, scope(info, namespace), name)? else {
            lines.push(not_found(info, name));
            continue;
        };
        let mut labels: BTreeMap<String, Value> = manifest["metadata"]["labels"]
            .as_object()
            .map(|l| l.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let before = labels.clone();
        for change in &args.changes {
            match change {
                LabelChange::Set(key, value) => {
                    if !args.overwrite {
                        if let Some(existing) = labels.get(key) {
                            return Ok(format!(
                                "error: '{key}' already has a value ({}), and --overwrite is false",
                                existing.as_str().unwrap_or_default()
                            ));
                        }
                    }
                    labels.insert(key.clone(), json!(value));
                }
                LabelChange::Remove(key) => {
                    labels.remove(key);
                }
            }
        }
        let prefix = info.name_prefix();
        if labels == before {
            lines.push(format!("{prefix}/{name} not labeled"));
            continue;
        }
        manifest["metadata"]["labels"] = json!(labels);
        match api::update(env.store, &manifest, namespace) {
            Ok(_) => lines.push(format!("{prefix}/{name} labeled")),
            Err(e) => lines.push(failed(e)?),
        }
    }
    Ok(lines.join("\n"))
}

// ---- expose ---------------------------------------------------------------------------------

pub fn expose(args: &ExposeArgs, env: &Env<'_>) -> Reply {
    let Some(info) = api::info(args.kind) else {
        return Ok(format!("error: unknown kind {}", args.kind));
    };
    let namespace = namespace_of(&args.namespace);
    let Some(source) = env.store.get(info.kind, namespace, &args.name)? else {
        return Ok(not_found(info, &args.name));
    };
    let singular = info.singular;

    let selector = match info.kind {
        "Pod" => source["metadata"]["labels"].clone(),
        "Service" => source["spec"]["selector"].clone(),
        _ => source["spec"]["selector"]["matchLabels"].clone(),
    };
    if selector.as_object().is_none_or(Map::is_empty) {
        return Ok(format!(
            "error: couldn't retrieve selectors via --selector flag or introspection: the {singular} has no labels and cannot be exposed"
        ));
    }
    let port = args.port.or_else(|| {
        let from = match info.kind {
            "Pod" => &source["spec"]["containers"],
            "Service" => {
                return source["spec"]["ports"][0]["port"]
                    .as_i64()
                    .and_then(|p| i32::try_from(p).ok())
            }
            _ => &source["spec"]["template"]["spec"]["containers"],
        };
        from.as_array()?
            .iter()
            .flat_map(|c| c["ports"].as_array().into_iter().flatten())
            .find_map(|p| p["containerPort"].as_i64())
            .and_then(|p| i32::try_from(p).ok())
    });
    let Some(port) = port else {
        return Ok("error: couldn't find port via --port flag or introspection".into());
    };
    let target = match args.target_port.as_deref() {
        Some(t) => t.parse::<i64>().map_or_else(|_| json!(t), |n| json!(n)),
        None => json!(port),
    };
    let name = args.service_name.clone().unwrap_or_else(|| args.name.clone());
    let mut spec = json!({
        "selector": selector,
        "ports": [{"port": port, "targetPort": target, "protocol": args.protocol.as_deref().unwrap_or("TCP")}],
    });
    if let Some(service_type) = &args.service_type {
        spec["type"] = json!(service_type);
    }
    let mut metadata = json!({"name": name});
    if source["metadata"]["labels"].as_object().is_some_and(|l| !l.is_empty()) && info.kind != "Service" {
        metadata["labels"] = source["metadata"]["labels"].clone();
    }
    let service = json!({"apiVersion": "v1", "kind": "Service", "metadata": metadata, "spec": spec});
    match api::create(env.store, &service, namespace, env.now, Origin::Client) {
        Ok(stored) => Ok(format!("{}/{} exposed", stored.info.name_prefix(), stored.name)),
        Err(e) => failed(e),
    }
}
