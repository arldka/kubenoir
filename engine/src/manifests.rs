//! `kubectl apply -f`, `create -f` and `delete -f`: manifests read from files and sent through
//! the API server's write path (`crate::api`).
//!
//! Like kubectl, the documents of every file are read and checked before anything is written,
//! results are printed per object, and server errors are collected after the successes.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::api::{self, ApiError, Origin};
use crate::controllers::Env;
use crate::mutate::delete_object;
use crate::parser::ManifestArgs;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

/// The annotation `kubectl apply` keeps its last configuration in.
pub const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

/// Text files the terminal can read, by path.
pub type Files = BTreeMap<String, String>;

/// One document of one file.
struct Doc<'a> {
    file: &'a str,
    manifest: Value,
    /// Namespace it goes to when its kind is namespaced.
    namespace: String,
}

impl Doc<'_> {
    fn name(&self) -> &str {
        self.manifest["metadata"]["name"].as_str().unwrap_or_default()
    }
}

/// The key a typed path is stored under: `./a.yaml` and `a.yaml` are the same file.
pub fn normalize_path(path: &str) -> String {
    let mut path = path;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.to_string()
}

/// Read and check every document of every file. `Err` is a whole-command error, printed alone.
fn read_docs<'a>(args: &'a ManifestArgs, files: &Files) -> Result<Vec<Doc<'a>>, String> {
    let mut docs = Vec::new();
    for file in &args.files {
        let text = files
            .get(&normalize_path(file))
            .ok_or_else(|| format!("error: the path \"{file}\" does not exist"))?;
        for document in serde_yaml_ng::Deserializer::from_str(text) {
            let value = Value::deserialize(document)
                .map_err(|e| format!("error: error parsing {file}: error converting YAML to JSON: {e}"))?;
            match value {
                Value::Null => {}
                list if list["kind"] == "List" => {
                    for item in list["items"].as_array().into_iter().flatten() {
                        docs.push(doc(args, file, item.clone())?);
                    }
                }
                value => docs.push(doc(args, file, value)?),
            }
        }
    }
    if docs.is_empty() {
        return Err("error: no objects passed to apply".into());
    }
    Ok(docs)
}

fn doc<'a>(args: &ManifestArgs, file: &'a str, manifest: Value) -> Result<Doc<'a>, String> {
    let kind_missing = manifest["kind"].as_str().is_none_or(str::is_empty);
    let api_version_missing = manifest["apiVersion"].as_str().is_none_or(str::is_empty);
    if !manifest.is_object() || kind_missing || api_version_missing {
        let error = ApiError::MissingTypeMeta {
            kind_missing,
            api_version_missing,
        };
        return Err(format!(
            "error: error validating \"{file}\": {}; if you choose to ignore these errors, turn validation off with --validate=false",
            error.message()
        ));
    }
    let own = manifest["metadata"]["namespace"].as_str().filter(|n| !n.is_empty());
    if let (Some(own), Some(flag)) = (own, args.namespace.as_deref()) {
        if own != flag {
            return Err(format!(
                "error: the namespace from the provided object \"{own}\" does not match the namespace \"{flag}\". You must pass '--namespace={own}' to perform this operation."
            ));
        }
    }
    let namespace = own
        .or(args.namespace.as_deref())
        .unwrap_or(DEFAULT_NAMESPACE)
        .to_string();
    Ok(Doc {
        file,
        manifest,
        namespace,
    })
}

/// How a failed write is reported: `Error from server (...): error when <verb> "<file>": ...`,
/// or kubectl's own message for kinds it cannot map.
fn write_error(doc: &Doc<'_>, verb: &str, error: &ApiError) -> String {
    match error {
        ApiError::NoKindMatch { .. } => format!(
            "error: resource mapping not found for name: \"{}\" namespace: \"{}\" from \"{}\": {}\nensure CRDs are installed first",
            doc.name(),
            doc.manifest["metadata"]["namespace"].as_str().unwrap_or_default(),
            doc.file,
            error.message()
        ),
        ApiError::Store(e) => format!("error: {e}"),
        _ => format!(
            "Error from server ({}): error when {verb} \"{}\": {}",
            error.reason().unwrap_or("InternalError"),
            doc.file,
            error.message()
        ),
    }
}

/// Successes, then errors: the order kubectl prints them in.
fn report(lines: Vec<String>, errors: Vec<String>) -> String {
    lines.into_iter().chain(errors).collect::<Vec<_>>().join("\n")
}

pub fn create(args: &ManifestArgs, env: &Env<'_>, files: &Files) -> Result<String, StoreError> {
    let docs = match read_docs(args, files) {
        Ok(docs) => docs,
        Err(message) => return Ok(message),
    };
    let (mut lines, mut errors) = (Vec::new(), Vec::new());
    for doc in &docs {
        match api::create(env.store, &doc.manifest, &doc.namespace, env.now, Origin::Client) {
            Ok(created) => lines.push(format!("{}/{} created", created.info.name_prefix(), created.name)),
            Err(e) => errors.push(write_error(doc, "creating", &e)),
        }
    }
    Ok(report(lines, errors))
}

pub fn apply(args: &ManifestArgs, env: &Env<'_>, files: &Files) -> Result<String, StoreError> {
    let docs = match read_docs(args, files) {
        Ok(docs) => docs,
        Err(message) => return Ok(message),
    };
    let (mut lines, mut errors) = (Vec::new(), Vec::new());
    for doc in &docs {
        match apply_one(doc, env) {
            Ok(mut out) => lines.append(&mut out),
            Err((verb, e)) => errors.push(write_error(doc, verb, &e)),
        }
    }
    Ok(report(lines, errors))
}

/// Apply one document: create it, or three-way merge it into what is stored. A failure says
/// which of the two it was (`creating` / `patching`).
fn apply_one(doc: &Doc<'_>, env: &Env<'_>) -> Result<Vec<String>, (&'static str, ApiError)> {
    let creating = |e| ("creating", e);
    let kind = doc.manifest["kind"].as_str().unwrap_or_default();
    let api_version = doc.manifest["apiVersion"].as_str().unwrap_or_default();
    let info = api::kind_for(kind, api_version).ok_or_else(|| {
        creating(ApiError::NoKindMatch {
            kind: kind.to_string(),
            api_version: api_version.to_string(),
        })
    })?;

    // What kubectl records as the configuration: the document as written, namespace filled in.
    let mut config = doc.manifest.clone();
    // `get_mut`, not indexing: indexing a missing key mutably inserts a `null` there.
    if let Some(meta) = config.get_mut("metadata").and_then(Value::as_object_mut) {
        if let Some(Value::Object(annotations)) = meta.get_mut("annotations") {
            annotations.remove(LAST_APPLIED);
            if annotations.is_empty() {
                meta.remove("annotations");
            }
        }
    }
    if info.namespaced {
        config["metadata"]["namespace"] = Value::String(doc.namespace.clone());
    }
    let mut desired = config.clone();
    let annotations = desired["metadata"]
        .as_object_mut()
        .map(|m| m.entry("annotations").or_insert_with(|| Value::Object(Map::new())));
    if let Some(Value::Object(annotations)) = annotations {
        annotations.insert(
            LAST_APPLIED.into(),
            Value::String(format!(
                "{}\n",
                serde_json::to_string(&config).map_err(|e| creating(e.into()))?
            )),
        );
    }

    let stored_namespace = if info.namespaced { doc.namespace.as_str() } else { "" };
    let live = env
        .store
        .get(info.kind, stored_namespace, doc.name())
        .map_err(|e| creating(e.into()))?;
    let Some(live) = live else {
        let created = api::create(env.store, &desired, &doc.namespace, env.now, Origin::Client).map_err(creating)?;
        return Ok(vec![format!("{}/{} created", info.name_prefix(), created.name)]);
    };

    let mut out = Vec::new();
    let last: Option<Value> = live["metadata"]["annotations"][LAST_APPLIED]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok());
    if last.is_none() {
        out.push(format!(
            "Warning: resource {}/{} is missing the {LAST_APPLIED} annotation which is required by kubectl apply. kubectl apply should only be used on resources created declaratively by either kubectl create --save-config or kubectl apply. The missing annotation will be patched automatically.",
            info.plural,
            doc.name()
        ));
    }
    let mut merged = live.clone();
    three_way_merge(&mut merged, last.as_ref(), &desired);
    let updated = api::update(env.store, &merged, &doc.namespace).map_err(|e| ("patching", e))?;
    let verb = if updated.changed { "configured" } else { "unchanged" };
    out.push(format!("{}/{} {verb}", info.name_prefix(), updated.name));
    Ok(out)
}

/// Merge `desired` into `live` the way `kubectl apply` does for plain JSON: fields of `desired`
/// win, fields the last applied configuration had but `desired` dropped are removed, and fields
/// only `live` has (set by controllers or defaults) stay. Lists are replaced whole.
fn three_way_merge(live: &mut Value, last: Option<&Value>, desired: &Value) {
    let (Some(live_fields), Some(desired_fields)) = (live.as_object_mut(), desired.as_object()) else {
        *live = desired.clone();
        return;
    };
    if let Some(last_fields) = last.and_then(Value::as_object) {
        for key in last_fields.keys() {
            if !desired_fields.contains_key(key) {
                live_fields.remove(key);
            }
        }
    }
    for (key, value) in desired_fields {
        let last_value = last.and_then(|l| l.get(key));
        match live_fields.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => three_way_merge(existing, last_value, value),
            _ => {
                live_fields.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn delete(args: &ManifestArgs, env: &Env<'_>, files: &Files) -> Result<String, StoreError> {
    let docs = match read_docs(args, files) {
        Ok(docs) => docs,
        Err(message) => return Ok(message),
    };
    let (mut lines, mut errors) = (Vec::new(), Vec::new());
    for doc in &docs {
        let kind = doc.manifest["kind"].as_str().unwrap_or_default();
        let api_version = doc.manifest["apiVersion"].as_str().unwrap_or_default();
        let Some(info) = api::kind_for(kind, api_version) else {
            let error = ApiError::NoKindMatch {
                kind: kind.to_string(),
                api_version: api_version.to_string(),
            };
            errors.push(write_error(doc, "deleting", &error));
            continue;
        };
        if info.kind == "Namespace" {
            errors.push("error: deleting namespaces is not available in this terminal".into());
            continue;
        }
        let namespace = if info.namespaced { doc.namespace.as_str() } else { "" };
        if delete_object(env, info.kind, namespace, doc.name(), false)? {
            lines.push(format!("{} \"{}\" deleted", info.name_prefix(), doc.name()));
        } else {
            let error = ApiError::NotFound {
                resource: info.resource(),
                name: doc.name().to_string(),
            };
            errors.push(write_error(doc, "deleting", &error));
        }
    }
    Ok(report(lines, errors))
}
