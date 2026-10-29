//! The simulated API server's write path: which kinds exist, and how a manifest becomes a stored
//! object.
//!
//! Every kind is backed by its `k8s-openapi` type. A write is checked the way kube-apiserver does
//! it: the kind must be known, required values present, the manifest must decode into the type
//! with no unknown fields (strict field validation), and server-side defaults are applied before
//! it is stored. A rejected write leaves the store untouched. Errors carry the reason and the
//! message kube-apiserver would return, so callers can wrap them the way kubectl does.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::clock;
use crate::names;
use crate::store::{ClusterStore, StoreError};

/// The namespaces every cluster starts with.
pub const SYSTEM_NAMESPACES: [&str; 4] = ["default", "kube-node-lease", "kube-public", "kube-system"];

/// How a resource kind is named and stored.
#[derive(Debug)]
pub struct KindInfo {
    pub kind: &'static str,
    pub api_version: &'static str,
    /// API group, empty for the core group.
    pub group: &'static str,
    /// Resource name (`pods`), as in URLs and kubectl messages.
    pub plural: &'static str,
    pub singular: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
    pub short_names: &'static [&'static str],
    /// Cluster-scoped kinds are stored with an empty namespace.
    pub namespaced: bool,
    /// Whether `metadata.generation` tracks spec changes.
    has_generation: bool,
    /// Whether a name must be a DNS-1123 label (no dots), not a subdomain.
    label_name: bool,
    /// Decode into the kind's type and encode back: what the API server keeps of a manifest.
    roundtrip: fn(&Value) -> Result<Value, String>,
}

impl KindInfo {
    /// Resource name qualified by its group, as kubectl prints it (`deployments.apps`).
    pub fn resource(&self) -> String {
        qualify(self.plural, self.group)
    }

    /// Prefix of `-o name` output and of kubectl's result lines (`deployment.apps`).
    pub fn name_prefix(&self) -> String {
        qualify(self.singular, self.group)
    }
}

fn qualify(name: &str, group: &str) -> String {
    if group.is_empty() {
        name.to_string()
    } else {
        format!("{name}.{group}")
    }
}

fn roundtrip<K: DeserializeOwned + Serialize>(manifest: &Value) -> Result<Value, String> {
    let typed: K = serde_json::from_value(manifest.clone()).map_err(|e| e.to_string())?;
    serde_json::to_value(typed).map_err(|e| e.to_string())
}

macro_rules! kind {
    ($ty:ty, $kind:literal, $group:literal, $av:literal, $plural:literal, $singular:literal,
     [$($short:literal),*], namespaced: $ns:literal, generation: $generation:literal, label: $label:literal) => {
        KindInfo {
            kind: $kind,
            api_version: $av,
            group: $group,
            plural: $plural,
            singular: $singular,
            short_names: &[$($short),*],
            namespaced: $ns,
            has_generation: $generation,
            label_name: $label,
            roundtrip: roundtrip::<$ty>,
        }
    };
}

use k8s_openapi::api::apps::v1 as apps;
use k8s_openapi::api::core::v1 as core;

static KINDS: [KindInfo; 9] = [
    kind!(core::ConfigMap, "ConfigMap", "", "v1", "configmaps", "configmap", ["cm"], namespaced: true, generation: false, label: false),
    kind!(apps::Deployment, "Deployment", "apps", "apps/v1", "deployments", "deployment", ["deploy"], namespaced: true, generation: true, label: false),
    kind!(core::Event, "Event", "", "v1", "events", "event", ["ev"], namespaced: true, generation: false, label: false),
    kind!(core::Namespace, "Namespace", "", "v1", "namespaces", "namespace", ["ns"], namespaced: false, generation: false, label: true),
    kind!(core::Node, "Node", "", "v1", "nodes", "node", ["no"], namespaced: false, generation: false, label: false),
    kind!(core::Pod, "Pod", "", "v1", "pods", "pod", ["po"], namespaced: true, generation: true, label: false),
    kind!(apps::ReplicaSet, "ReplicaSet", "apps", "apps/v1", "replicasets", "replicaset", ["rs"], namespaced: true, generation: true, label: false),
    kind!(core::Secret, "Secret", "", "v1", "secrets", "secret", [], namespaced: true, generation: false, label: false),
    kind!(core::Service, "Service", "", "v1", "services", "service", ["svc"], namespaced: true, generation: false, label: true),
];

/// Every known kind, sorted by kind name.
pub fn kinds() -> &'static [KindInfo] {
    &KINDS
}

/// The kind a kubectl resource argument names: plural, singular, short name or kind, any case,
/// optionally qualified by group (`deployments.apps`).
pub fn kind_for_arg(arg: &str) -> Option<&'static KindInfo> {
    let arg = arg.to_ascii_lowercase();
    let (name, group) = match arg.split_once('.') {
        Some((name, group)) => (name.to_string(), Some(group.to_string())),
        None => (arg, None),
    };
    KINDS.iter().find(|k| {
        group.as_deref().is_none_or(|g| g == k.group)
            && (name == k.plural
                || name == k.singular
                || name == k.kind.to_ascii_lowercase()
                || k.short_names.contains(&name.as_str()))
    })
}

/// The kind a manifest declares, by `kind` and `apiVersion`.
pub fn kind_for(kind: &str, api_version: &str) -> Option<&'static KindInfo> {
    KINDS.iter().find(|k| k.kind == kind && k.api_version == api_version)
}

#[derive(Debug)]
pub enum ApiError {
    /// `kind` / `apiVersion` missing from the manifest.
    MissingTypeMeta {
        kind_missing: bool,
        api_version_missing: bool,
    },
    /// No such kind in that API version.
    NoKindMatch {
        kind: String,
        api_version: String,
    },
    /// The manifest does not decode into its kind (wrong type, unknown field).
    BadRequest {
        kind: &'static str,
        api_version: &'static str,
        reason: String,
    },
    /// The manifest decodes but breaks a validation rule.
    Invalid {
        kind: &'static str,
        name: String,
        causes: Vec<String>,
    },
    NotFound {
        resource: String,
        name: String,
    },
    AlreadyExists {
        resource: String,
        name: String,
    },
    Store(StoreError),
}

impl ApiError {
    /// Reason in `Error from server (<reason>)`; `None` for errors kubectl raises itself.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            ApiError::MissingTypeMeta { .. } | ApiError::NoKindMatch { .. } | ApiError::Store(_) => None,
            ApiError::BadRequest { .. } => Some("BadRequest"),
            ApiError::Invalid { .. } => Some("Invalid"),
            ApiError::NotFound { .. } => Some("NotFound"),
            ApiError::AlreadyExists { .. } => Some("AlreadyExists"),
        }
    }

    /// The message alone, without the `Error from server (...)` prefix.
    pub fn message(&self) -> String {
        match self {
            ApiError::MissingTypeMeta {
                kind_missing,
                api_version_missing,
            } => {
                let missing: Vec<&str> = [
                    (*api_version_missing, "apiVersion not set"),
                    (*kind_missing, "kind not set"),
                ]
                .into_iter()
                .filter_map(|(m, text)| m.then_some(text))
                .collect();
                format!("error validating data: [{}]", missing.join(", "))
            }
            ApiError::NoKindMatch { kind, api_version } => {
                format!("no matches for kind \"{kind}\" in version \"{api_version}\"")
            }
            ApiError::BadRequest {
                kind,
                api_version,
                reason,
            } => format!(
                "{kind} in version \"{}\" cannot be handled as a {kind}: {reason}",
                version_of(api_version)
            ),
            ApiError::Invalid { kind, name, causes } => match causes.as_slice() {
                [one] => format!("The {kind} \"{name}\" is invalid: {one}"),
                many => format!(
                    "The {kind} \"{name}\" is invalid: \n{}",
                    many.iter().map(|c| format!("* {c}")).collect::<Vec<_>>().join("\n")
                ),
            },
            ApiError::NotFound { resource, name } => format!("{resource} \"{name}\" not found"),
            ApiError::AlreadyExists { resource, name } => format!("{resource} \"{name}\" already exists"),
            ApiError::Store(e) => e.to_string(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason() {
            Some(reason) => write!(f, "Error from server ({reason}): {}", self.message()),
            None => write!(f, "error: {}", self.message()),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        ApiError::Store(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::Store(StoreError::Json(e))
    }
}

fn version_of(api_version: &str) -> &str {
    api_version.rsplit('/').next().unwrap_or(api_version)
}

/// How a write treats the server-owned parts of a manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Origin {
    /// A client write: `status`, `uid` and `creationTimestamp` are the server's, not the client's.
    #[default]
    Client,
    /// Authored cluster state (a scenario's manifests): keep what the author wrote, so objects
    /// can carry a status and a past.
    Authored,
}

/// An object as stored.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Stored {
    pub info: &'static KindInfo,
    pub namespace: String,
    pub name: String,
    pub manifest: Value,
    /// False when an update asked for exactly what was already stored (nothing was written).
    pub changed: bool,
}

/// Create the namespaces every cluster has. Idempotent.
pub fn seed_system_namespaces(store: &ClusterStore, now: i64) -> Result<(), ApiError> {
    for name in SYSTEM_NAMESPACES {
        if store.get("Namespace", "", name)?.is_none() {
            create(
                store,
                &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name}}),
                "default",
                now,
                Origin::Client,
            )?;
        }
    }
    Ok(())
}

/// Create an object. `namespace` applies when the manifest names none (ignored for
/// cluster-scoped kinds); the namespace must exist.
pub fn create(
    store: &ClusterStore,
    manifest: &Value,
    namespace: &str,
    now: i64,
    origin: Origin,
) -> Result<Stored, ApiError> {
    let (info, mut object) = admit(manifest, namespace)?;
    let namespace = object_namespace(info, &object);
    ensure_namespace(store, info, &namespace)?;

    let meta = object["metadata"].as_object_mut().expect("admit ensures metadata");
    if meta.get("name").and_then(Value::as_str).is_none_or(str::is_empty) {
        if let Some(prefix) = meta.get("generateName").and_then(Value::as_str) {
            let name = format!("{prefix}{}", names::pod_suffix(store.seed(), store.next_id()));
            meta.insert("name".into(), Value::String(name));
        }
    }
    let name = meta.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
    if origin == Origin::Client {
        meta.remove("uid");
        meta.remove("creationTimestamp");
        meta.remove("deletionTimestamp");
        meta.remove("resourceVersion");
        meta.remove("generation");
        object.as_object_mut().expect("object").remove("status");
    }
    validate(info, &name, &object)?;

    let meta = object["metadata"].as_object_mut().expect("object");
    meta.entry("creationTimestamp")
        .or_insert_with(|| Value::String(clock::format(now)));
    if info.has_generation {
        meta.entry("generation").or_insert(json!(1));
    }
    apply_defaults(info, &name, &mut object);

    if store.get(info.kind, &namespace, &name)?.is_some() {
        return Err(ApiError::AlreadyExists {
            resource: info.resource(),
            name,
        });
    }
    store.insert(info.kind, info.api_version, &namespace, &name, &object)?;
    stored(store, info, namespace, name)
}

/// Replace an existing object with a new desired state (`kubectl replace`, `apply`, `edit`).
/// Identity, creation time and status stay the server's; `metadata.generation` moves on when
/// anything outside `metadata` and `status` changed.
pub fn update(store: &ClusterStore, manifest: &Value, namespace: &str) -> Result<Stored, ApiError> {
    let (info, mut object) = admit(manifest, namespace)?;
    let namespace = object_namespace(info, &object);
    let name = object["metadata"]["name"].as_str().unwrap_or_default().to_string();
    if name.is_empty() {
        return Err(ApiError::Invalid {
            kind: info.kind,
            name,
            causes: vec!["metadata.name: Required value: name is required".into()],
        });
    }
    let Some(current) = store.get(info.kind, &namespace, &name)? else {
        return Err(ApiError::NotFound {
            resource: info.resource(),
            name,
        });
    };
    validate(info, &name, &object)?;
    apply_defaults(info, &name, &mut object);
    validate_update(info, &name, &object, &current)?;

    let fields = object.as_object_mut().expect("object");
    match current.get("status") {
        Some(status) => fields.insert("status".into(), status.clone()),
        None => fields.remove("status"),
    };
    let meta = object["metadata"].as_object_mut().expect("object");
    for key in ["uid", "creationTimestamp", "deletionTimestamp", "generation"] {
        match current["metadata"].get(key) {
            Some(value) => meta.insert(key.into(), value.clone()),
            None => meta.remove(key),
        };
    }
    // Ownership is the controllers' bookkeeping; a client that does not mention it keeps it.
    if let Some(owners) = current["metadata"].get("ownerReferences") {
        meta.entry("ownerReferences").or_insert_with(|| owners.clone());
    }
    if info.has_generation && spec_of(&object) != spec_of(&current) {
        let generation = current["metadata"]["generation"].as_i64().unwrap_or(1) + 1;
        object["metadata"]["generation"] = json!(generation);
    }
    // Identical desired state is not a write: no new resourceVersion (`kubectl apply` says
    // "unchanged").
    if let Some(version) = current["metadata"].get("resourceVersion") {
        object["metadata"]["resourceVersion"] = version.clone();
    }
    if object == current {
        return Ok(Stored {
            info,
            namespace,
            name,
            manifest: current,
            changed: false,
        });
    }
    store.update(info.kind, &namespace, &name, &object)?;
    stored(store, info, namespace, name)
}

/// Everything but `metadata` and `status`: what a generation counts changes of.
fn spec_of(object: &Value) -> Map<String, Value> {
    let mut fields = object.as_object().cloned().unwrap_or_default();
    fields.remove("metadata");
    fields.remove("status");
    fields
}

fn stored(store: &ClusterStore, info: &'static KindInfo, namespace: String, name: String) -> Result<Stored, ApiError> {
    let manifest = store.get(info.kind, &namespace, &name)?.unwrap_or(Value::Null);
    Ok(Stored {
        info,
        namespace,
        name,
        manifest,
        changed: true,
    })
}

fn object_namespace(info: &KindInfo, object: &Value) -> String {
    if info.namespaced {
        object["metadata"]["namespace"].as_str().unwrap_or_default().to_string()
    } else {
        String::new()
    }
}

fn ensure_namespace(store: &ClusterStore, info: &KindInfo, namespace: &str) -> Result<(), ApiError> {
    if info.namespaced && store.get("Namespace", "", namespace)?.is_none() {
        return Err(ApiError::NotFound {
            resource: "namespaces".into(),
            name: namespace.to_string(),
        });
    }
    Ok(())
}

/// Resolve the kind and decode strictly: the object as the server keeps it, with
/// `metadata.namespace` set for namespaced kinds and removed for cluster-scoped ones.
fn admit(manifest: &Value, namespace: &str) -> Result<(&'static KindInfo, Value), ApiError> {
    let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or_default();
    let api_version = manifest.get("apiVersion").and_then(Value::as_str).unwrap_or_default();
    if kind.is_empty() || api_version.is_empty() {
        return Err(ApiError::MissingTypeMeta {
            kind_missing: kind.is_empty(),
            api_version_missing: api_version.is_empty(),
        });
    }
    let info = kind_for(kind, api_version).ok_or_else(|| ApiError::NoKindMatch {
        kind: kind.to_string(),
        api_version: api_version.to_string(),
    })?;
    let bad_request = |reason: String| ApiError::BadRequest {
        kind: info.kind,
        api_version: info.api_version,
        reason,
    };

    let mut object = (info.roundtrip)(manifest).map_err(bad_request)?;
    if let Some(field) = first_unknown_field(manifest, &object, "") {
        return Err(bad_request(format!("strict decoding error: unknown field \"{field}\"")));
    }
    let fields = object
        .as_object_mut()
        .ok_or_else(|| bad_request("not an object".into()))?;
    let meta = fields
        .entry("metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| bad_request("metadata is not an object".into()))?;
    if info.namespaced {
        meta.entry("namespace")
            .or_insert_with(|| Value::String(namespace.to_string()));
    } else {
        meta.remove("namespace");
    }
    Ok((info, object))
}

/// The first field of `input` (dotted path) that did not survive decoding: an unknown field.
/// `null` values count as absent, as they do for the API server.
fn first_unknown_field(input: &Value, decoded: &Value, path: &str) -> Option<String> {
    match (input, decoded) {
        (Value::Object(fields), Value::Object(kept)) => fields.iter().find_map(|(key, value)| {
            if value.is_null() {
                return None;
            }
            let here = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match kept.get(key) {
                None => Some(here),
                Some(kept_value) => first_unknown_field(value, kept_value, &here),
            }
        }),
        (Value::Array(items), Value::Array(kept)) => items
            .iter()
            .zip(kept)
            .enumerate()
            .find_map(|(i, (item, kept_item))| first_unknown_field(item, kept_item, &format!("{path}[{i}]"))),
        _ => None,
    }
}

// ---- validation -------------------------------------------------------------------------------

const SUBDOMAIN_HELP: &str = "a lowercase RFC 1123 subdomain must consist of lower case alphanumeric characters, '-' or '.', and must start and end with an alphanumeric character (e.g. 'example.com', regex used for validation is '[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*')";
const LABEL_HELP: &str = "a lowercase RFC 1123 label must consist of lower case alphanumeric characters or '-', and must start and end with an alphanumeric character (e.g. 'my-name',  or '123-abc', regex used for validation is '[a-z0-9]([-a-z0-9]*[a-z0-9])?')";

fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn is_dns_subdomain(s: &str) -> bool {
    s.len() <= 253 && s.split('.').all(is_dns_label)
}

fn validate(info: &KindInfo, name: &str, object: &Value) -> Result<(), ApiError> {
    let mut causes = Vec::new();
    if name.is_empty() {
        causes.push("metadata.name: Required value: name or generateName is required".to_string());
    } else if info.label_name && !is_dns_label(name) {
        causes.push(format!("metadata.name: Invalid value: \"{name}\": {LABEL_HELP}"));
    } else if !info.label_name && !is_dns_subdomain(name) {
        causes.push(format!("metadata.name: Invalid value: \"{name}\": {SUBDOMAIN_HELP}"));
    }
    match info.kind {
        "Pod" => validate_pod_spec(&object["spec"], "spec", &mut causes),
        "Deployment" | "ReplicaSet" => validate_workload(&object["spec"], &mut causes),
        "Service" => validate_service(&object["spec"], &mut causes),
        _ => {}
    }
    if causes.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Invalid {
            kind: info.kind,
            name: name.to_string(),
            causes,
        })
    }
}

fn validate_pod_spec(spec: &Value, path: &str, causes: &mut Vec<String>) {
    let containers = spec["containers"].as_array().map(Vec::as_slice).unwrap_or_default();
    if containers.is_empty() {
        causes.push(format!("{path}.containers: Required value"));
    }
    for (field, list) in [
        ("containers", containers),
        (
            "initContainers",
            spec["initContainers"].as_array().map(Vec::as_slice).unwrap_or_default(),
        ),
    ] {
        for (i, container) in list.iter().enumerate() {
            let name = container["name"].as_str().unwrap_or_default();
            if name.is_empty() {
                causes.push(format!("{path}.{field}[{i}].name: Required value"));
            } else if !is_dns_label(name) {
                causes.push(format!(
                    "{path}.{field}[{i}].name: Invalid value: \"{name}\": {LABEL_HELP}"
                ));
            }
            if container["image"].as_str().unwrap_or_default().is_empty() {
                causes.push(format!("{path}.{field}[{i}].image: Required value"));
            }
        }
    }
}

fn validate_workload(spec: &Value, causes: &mut Vec<String>) {
    let selector = spec["selector"]["matchLabels"].as_object();
    let has_expressions = spec["selector"]["matchExpressions"]
        .as_array()
        .is_some_and(|e| !e.is_empty());
    if selector.is_none_or(Map::is_empty) && !has_expressions {
        causes.push("spec.selector: Required value".into());
    }
    let labels = spec["template"]["metadata"]["labels"].as_object();
    if let Some(selector) = selector {
        let matches = selector.iter().all(|(k, v)| labels.and_then(|l| l.get(k)) == Some(v));
        if !matches {
            causes.push(format!(
                "spec.template.metadata.labels: Invalid value: {}: `selector` does not match template `labels`",
                go_map(labels)
            ));
        }
    }
    validate_pod_spec(&spec["template"]["spec"], "spec.template.spec", causes);
}

fn validate_service(spec: &Value, causes: &mut Vec<String>) {
    let kind = spec["type"].as_str().unwrap_or("ClusterIP");
    let ports = spec["ports"].as_array().map(Vec::as_slice).unwrap_or_default();
    if kind != "ExternalName" && ports.is_empty() {
        causes.push("spec.ports: Required value".into());
    }
    if kind == "ExternalName" && spec["externalName"].as_str().unwrap_or_default().is_empty() {
        causes.push("spec.externalName: Required value".into());
    }
}

/// A label map the way Go prints it in validation messages.
fn go_map(labels: Option<&Map<String, Value>>) -> String {
    let entries: Vec<String> = labels
        .into_iter()
        .flatten()
        .map(|(k, v)| format!("{k:?}:{:?}", v.as_str().unwrap_or_default()))
        .collect();
    format!("map[string]string{{{}}}", entries.join(", "))
}

/// What kube-apiserver says when a pod update touches anything but the few mutable fields.
pub const POD_UPDATE_FORBIDDEN: &str = "spec: Forbidden: pod updates may not change fields other than `spec.containers[*].image`, `spec.initContainers[*].image`, `spec.activeDeadlineSeconds`, `spec.tolerations` (only additions to existing tolerations), `spec.terminationGracePeriodSeconds` (allow it to be set to 1 if it was previously negative)";

/// Rules about how an existing object may change (both sides already defaulted).
fn validate_update(info: &KindInfo, name: &str, object: &Value, current: &Value) -> Result<(), ApiError> {
    let mut causes = Vec::new();
    match info.kind {
        "Pod" => {
            if !pod_spec_change_allowed(&current["spec"], &object["spec"]) {
                causes.push(POD_UPDATE_FORBIDDEN.to_string());
            }
        }
        "Deployment" | "ReplicaSet" => {
            let selector = &object["spec"]["selector"];
            if *selector != current["spec"]["selector"] {
                causes.push(format!(
                    "spec.selector: Invalid value: {}: field is immutable",
                    go_selector(selector)
                ));
            }
        }
        _ => {}
    }
    if causes.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Invalid {
            kind: info.kind,
            name: name.to_string(),
            causes,
        })
    }
}

/// Whether `new` differs from `old` only where a running pod may change: container and init
/// container images, `activeDeadlineSeconds`, tolerations added at the end, and a grace period
/// that was negative becoming 1.
fn pod_spec_change_allowed(old: &Value, new: &Value) -> bool {
    let mut old = old.clone();
    let mut new = new.clone();
    for field in ["containers", "initContainers"] {
        let old_list = old[field].as_array().map_or(0, Vec::len);
        let new_list = new[field].as_array().map_or(0, Vec::len);
        if old_list != new_list {
            return false;
        }
        for i in 0..old_list {
            new[field][i]["image"] = old[field][i]["image"].clone();
        }
    }
    new["activeDeadlineSeconds"] = old["activeDeadlineSeconds"].clone();
    let old_tolerations = old["tolerations"].as_array().cloned().unwrap_or_default();
    let new_tolerations = new["tolerations"].as_array().cloned().unwrap_or_default();
    if new_tolerations.len() < old_tolerations.len() || new_tolerations[..old_tolerations.len()] != old_tolerations[..]
    {
        return false;
    }
    new["tolerations"] = old["tolerations"].clone();
    let old_grace = old["terminationGracePeriodSeconds"].as_i64().unwrap_or(30);
    if old_grace < 0 && new["terminationGracePeriodSeconds"] == json!(1) {
        new["terminationGracePeriodSeconds"] = old["terminationGracePeriodSeconds"].clone();
    }
    // A field set to null and a field left out are the same to the API server.
    strip_nulls(&mut old);
    strip_nulls(&mut new);
    old == new
}

fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, v| !v.is_null());
            fields.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// A label selector the way Go prints it in validation messages (`v1.LabelSelector{...}`).
fn go_selector(selector: &Value) -> String {
    format!(
        "v1.LabelSelector{{MatchLabels:{}, MatchExpressions:[]v1.LabelSelectorRequirement(nil)}}",
        go_map(selector["matchLabels"].as_object())
    )
}

// ---- defaulting -------------------------------------------------------------------------------

fn set_default(fields: &mut Map<String, Value>, key: &str, value: Value) {
    fields.entry(key).or_insert(value);
}

fn apply_defaults(info: &KindInfo, name: &str, object: &mut Value) {
    match info.kind {
        "Pod" => default_pod_spec(&mut object["spec"]),
        "Deployment" => {
            let spec = obj(&mut object["spec"]);
            set_default(spec, "replicas", json!(1));
            set_default(spec, "revisionHistoryLimit", json!(10));
            set_default(spec, "progressDeadlineSeconds", json!(600));
            let strategy = obj(spec.entry("strategy").or_insert_with(|| json!({})));
            set_default(strategy, "type", json!("RollingUpdate"));
            if strategy["type"] == "RollingUpdate" {
                let rolling = obj(strategy.entry("rollingUpdate").or_insert_with(|| json!({})));
                set_default(rolling, "maxSurge", json!("25%"));
                set_default(rolling, "maxUnavailable", json!("25%"));
            }
            default_pod_spec(&mut object["spec"]["template"]["spec"]);
        }
        "ReplicaSet" => {
            set_default(obj(&mut object["spec"]), "replicas", json!(1));
            default_pod_spec(&mut object["spec"]["template"]["spec"]);
        }
        "Service" => {
            let spec = obj(&mut object["spec"]);
            set_default(spec, "type", json!("ClusterIP"));
            set_default(spec, "sessionAffinity", json!("None"));
            if let Some(ports) = spec.get_mut("ports").and_then(Value::as_array_mut) {
                for port in ports {
                    let port = obj(port);
                    set_default(port, "protocol", json!("TCP"));
                    if let Some(number) = port.get("port").cloned() {
                        set_default(port, "targetPort", number);
                    }
                }
            }
        }
        "Namespace" => {
            let labels = obj(obj(&mut object["metadata"])
                .entry("labels")
                .or_insert_with(|| json!({})));
            labels.insert("kubernetes.io/metadata.name".into(), json!(name));
            set_default(obj(&mut object["spec"]), "finalizers", json!(["kubernetes"]));
            set_default(obj(&mut object["status"]), "phase", json!("Active"));
        }
        "Secret" => set_default(obj(object), "type", json!("Opaque")),
        _ => {}
    }
}

/// `value` as an object, turning it into an empty one first if it is anything else.
fn obj(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().expect("just made an object")
}

fn default_pod_spec(spec: &mut Value) {
    let spec = obj(spec);
    set_default(spec, "restartPolicy", json!("Always"));
    set_default(spec, "dnsPolicy", json!("ClusterFirst"));
    set_default(spec, "schedulerName", json!("default-scheduler"));
    set_default(spec, "terminationGracePeriodSeconds", json!(30));
    set_default(spec, "securityContext", json!({}));
    for field in ["containers", "initContainers"] {
        let Some(containers) = spec.get_mut(field).and_then(Value::as_array_mut) else {
            continue;
        };
        for container in containers {
            let container = obj(container);
            let image = container.get("image").and_then(Value::as_str).unwrap_or_default();
            let pull = if image_tag(image).is_none_or(|t| t == "latest") {
                "Always"
            } else {
                "IfNotPresent"
            };
            set_default(container, "imagePullPolicy", json!(pull));
            set_default(container, "terminationMessagePath", json!("/dev/termination-log"));
            set_default(container, "terminationMessagePolicy", json!("File"));
            set_default(container, "resources", json!({}));
            if let Some(ports) = container.get_mut("ports").and_then(Value::as_array_mut) {
                for port in ports {
                    set_default(obj(port), "protocol", json!("TCP"));
                }
            }
        }
    }
}

/// The tag of an image reference, if it has one (`nginx:1.27` → `1.27`; digests have none).
fn image_tag(image: &str) -> Option<&str> {
    if image.contains('@') {
        return None;
    }
    let last = image.rsplit('/').next().unwrap_or(image);
    last.split_once(':').map(|(_, tag)| tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_125_800;

    fn store() -> ClusterStore {
        let store = ClusterStore::new().unwrap();
        seed_system_namespaces(&store, NOW).unwrap();
        store
    }

    fn pod(name: &str) -> Value {
        json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name},
               "spec": {"containers": [{"name": "app", "image": "nginx:1.27"}]}})
    }

    fn deployment(name: &str) -> Value {
        json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": name},
               "spec": {"selector": {"matchLabels": {"app": name}},
                        "template": {"metadata": {"labels": {"app": name}},
                                     "spec": {"containers": [{"name": "app", "image": "web:2"}]}}}})
    }

    fn create_client(store: &ClusterStore, manifest: &Value) -> Result<Stored, ApiError> {
        create(store, manifest, "default", NOW, Origin::Client)
    }

    #[test]
    fn resolves_kubectl_resource_arguments() {
        for arg in ["po", "pod", "pods", "Pod", "PODS"] {
            assert_eq!(kind_for_arg(arg).unwrap().kind, "Pod", "{arg}");
        }
        assert_eq!(kind_for_arg("deployments.apps").unwrap().kind, "Deployment");
        assert_eq!(kind_for_arg("deploy").unwrap().resource(), "deployments.apps");
        assert_eq!(kind_for_arg("ns").unwrap().kind, "Namespace");
        assert!(kind_for_arg("pods.apps").is_none());
        assert!(kind_for_arg("widgets").is_none());
        assert_eq!(kind_for_arg("svc").unwrap().name_prefix(), "service");
    }

    #[test]
    fn system_namespaces_exist_and_are_cluster_scoped() {
        let store = store();
        for ns in SYSTEM_NAMESPACES {
            let stored = store.get("Namespace", "", ns).unwrap().unwrap();
            assert_eq!(stored["status"]["phase"], "Active");
            assert_eq!(stored["metadata"]["labels"]["kubernetes.io/metadata.name"], ns);
            assert!(stored["metadata"].get("namespace").is_none());
        }
        seed_system_namespaces(&store, NOW).unwrap();
        assert_eq!(store.list("Namespace", "").unwrap().len(), 4);
    }

    #[test]
    fn creates_with_server_side_defaults() {
        let store = store();
        let created = create_client(&store, &pod("web")).unwrap();
        assert_eq!(created.info.kind, "Pod");
        assert_eq!(created.manifest, store.get("Pod", "default", "web").unwrap().unwrap());
        assert_eq!((created.namespace.as_str(), created.name.as_str()), ("default", "web"));
        let m = store.get("Pod", "default", "web").unwrap().unwrap();
        assert_eq!(m["metadata"]["namespace"], "default");
        assert_eq!(m["metadata"]["creationTimestamp"], clock::format(NOW));
        assert_eq!(m["metadata"]["generation"], 1);
        assert_eq!(m["metadata"]["uid"].as_str().unwrap().len(), 36);
        assert_eq!(m["spec"]["restartPolicy"], "Always");
        assert_eq!(m["spec"]["dnsPolicy"], "ClusterFirst");
        assert_eq!(m["spec"]["containers"][0]["imagePullPolicy"], "IfNotPresent");
        assert_eq!(
            m["spec"]["containers"][0]["terminationMessagePath"],
            "/dev/termination-log"
        );

        create_client(&store, &deployment("api")).unwrap();
        let d = store.get("Deployment", "default", "api").unwrap().unwrap();
        assert_eq!(d["spec"]["replicas"], 1);
        assert_eq!(d["spec"]["strategy"]["rollingUpdate"]["maxSurge"], "25%");
        assert_eq!(d["spec"]["template"]["spec"]["restartPolicy"], "Always");
    }

    #[test]
    fn latest_or_untagged_images_are_always_pulled() {
        let store = store();
        let mut manifest = pod("a");
        manifest["spec"]["containers"][0]["image"] = json!("registry:5000/team/app");
        create_client(&store, &manifest).unwrap();
        let m = store.get("Pod", "default", "a").unwrap().unwrap();
        assert_eq!(m["spec"]["containers"][0]["imagePullPolicy"], "Always");
    }

    #[test]
    fn explicit_values_win_over_defaults() {
        let store = store();
        let mut manifest = deployment("api");
        manifest["spec"]["replicas"] = json!(3);
        manifest["spec"]["strategy"] = json!({"type": "Recreate"});
        create_client(&store, &manifest).unwrap();
        let d = store.get("Deployment", "default", "api").unwrap().unwrap();
        assert_eq!(d["spec"]["replicas"], 3);
        assert_eq!(d["spec"]["strategy"], json!({"type": "Recreate"}));
    }

    #[test]
    fn client_writes_drop_server_owned_fields() {
        let store = store();
        let mut manifest = pod("web");
        manifest["metadata"]["uid"] = json!("mine");
        manifest["metadata"]["creationTimestamp"] = json!("2020-01-01T00:00:00Z");
        manifest["status"] = json!({"phase": "Running"});
        create_client(&store, &manifest).unwrap();
        let m = store.get("Pod", "default", "web").unwrap().unwrap();
        assert_ne!(m["metadata"]["uid"], "mine");
        assert_eq!(m["metadata"]["creationTimestamp"], clock::format(NOW));
        assert!(m.get("status").is_none());
    }

    #[test]
    fn authored_objects_keep_their_status_and_past() {
        let store = store();
        let mut manifest = pod("web");
        manifest["metadata"]["creationTimestamp"] = json!("2026-10-24T18:30:00Z");
        manifest["status"] = json!({"phase": "Running"});
        create(&store, &manifest, "default", NOW, Origin::Authored).unwrap();
        let m = store.get("Pod", "default", "web").unwrap().unwrap();
        assert_eq!(m["metadata"]["creationTimestamp"], "2026-10-24T18:30:00Z");
        assert_eq!(m["status"]["phase"], "Running");
    }

    #[test]
    fn unknown_fields_are_rejected_and_nothing_is_stored() {
        let store = store();
        let mut manifest = pod("web");
        manifest["spec"]["containers"][0]["imagee"] = json!("typo");
        let err = create_client(&store, &manifest).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error from server (BadRequest): Pod in version \"v1\" cannot be handled as a Pod: strict decoding error: unknown field \"spec.containers[0].imagee\""
        );
        assert!(store.get("Pod", "default", "web").unwrap().is_none());
    }

    #[test]
    fn nulls_are_not_unknown_fields() {
        let store = store();
        let mut manifest = pod("web");
        manifest["metadata"]["creationTimestamp"] = Value::Null;
        manifest["spec"]["containers"][0]["resources"] = json!({"limits": {"cpu": 1}});
        create_client(&store, &manifest).unwrap();
    }

    #[test]
    fn wrong_types_are_bad_requests() {
        let store = store();
        let mut manifest = pod("web");
        manifest["spec"]["containers"][0]["image"] = json!(3);
        let err = create_client(&store, &manifest).unwrap_err();
        assert_eq!(err.reason(), Some("BadRequest"));
        assert!(
            err.message()
                .starts_with("Pod in version \"v1\" cannot be handled as a Pod: "),
            "{err}"
        );
    }

    #[test]
    fn unknown_kinds_and_missing_type_meta() {
        let store = store();
        let err = create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Widget", "metadata": {"name": "a"}}),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "error: no matches for kind \"Widget\" in version \"v1\""
        );
        let err = create_client(&store, &json!({"apiVersion": "v1", "kind": "Deployment"})).unwrap_err();
        assert!(matches!(err, ApiError::NoKindMatch { .. }), "{err}");
        let err = create_client(&store, &json!({"metadata": {"name": "a"}})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "error: error validating data: [apiVersion not set, kind not set]"
        );
    }

    #[test]
    fn required_values_and_names_are_validated() {
        let store = store();
        let err = create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "Web"}, "spec": {"containers": [{"name": "app"}]}}),
        )
        .unwrap_err();
        assert_eq!(err.reason(), Some("Invalid"));
        let message = err.message();
        assert!(
            message.starts_with(
                "The Pod \"Web\" is invalid: \n* metadata.name: Invalid value: \"Web\": a lowercase RFC 1123 subdomain"
            ),
            "{message}"
        );
        assert!(
            message.ends_with("\n* spec.containers[0].image: Required value"),
            "{message}"
        );

        let err = create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Pod", "metadata": {}, "spec": {"containers": []}}),
        )
        .unwrap_err();
        assert!(
            err.message()
                .contains("metadata.name: Required value: name or generateName is required"),
            "{err}"
        );
        assert!(err.message().contains("spec.containers: Required value"), "{err}");

        let err = create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "a.b"}}),
        )
        .unwrap_err();
        assert!(err.message().contains("a lowercase RFC 1123 label"), "{err}");
    }

    #[test]
    fn workload_selector_must_match_template_labels() {
        let store = store();
        let mut manifest = deployment("api");
        manifest["spec"]["template"]["metadata"]["labels"] = json!({"app": "other"});
        let err = create_client(&store, &manifest).unwrap_err();
        assert_eq!(
            err.message(),
            "The Deployment \"api\" is invalid: spec.template.metadata.labels: Invalid value: map[string]string{\"app\":\"other\"}: `selector` does not match template `labels`"
        );
        manifest["spec"].as_object_mut().unwrap().remove("selector");
        let err = create_client(&store, &manifest).unwrap_err();
        assert!(err.message().contains("spec.selector: Required value"), "{err}");
    }

    #[test]
    fn namespace_must_exist_except_for_cluster_scoped_kinds() {
        let store = store();
        let err = create(&store, &pod("web"), "homicide", NOW, Origin::Client).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error from server (NotFound): namespaces \"homicide\" not found"
        );
        assert!(store.get("Pod", "homicide", "web").unwrap().is_none());

        create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "homicide", "namespace": "ignored"}}),
        )
        .unwrap();
        let created = create(&store, &pod("web"), "homicide", NOW, Origin::Client).unwrap();
        assert_eq!(created.namespace, "homicide");
        let mut elsewhere = pod("other");
        elsewhere["metadata"]["namespace"] = json!("kube-system");
        assert_eq!(
            create(&store, &elsewhere, "homicide", NOW, Origin::Client)
                .unwrap()
                .namespace,
            "kube-system"
        );

        let node = create_client(
            &store,
            &json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "worker-1"}}),
        )
        .unwrap();
        assert_eq!(node.namespace, "");
        assert!(store.get("Node", "", "worker-1").unwrap().is_some());
    }

    #[test]
    fn duplicates_are_already_exists() {
        let store = store();
        create_client(&store, &deployment("api")).unwrap();
        let err = create_client(&store, &deployment("api")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error from server (AlreadyExists): deployments.apps \"api\" already exists"
        );
    }

    #[test]
    fn generate_name_gets_a_suffix() {
        let store = store();
        let mut manifest = pod("");
        manifest["metadata"] = json!({"generateName": "job-"});
        let created = create_client(&store, &manifest).unwrap();
        assert!(
            created.name.starts_with("job-") && created.name.len() == 9,
            "{}",
            created.name
        );
    }

    #[test]
    fn update_keeps_identity_and_status_and_bumps_generation_on_spec_change() {
        let store = store();
        create_client(&store, &deployment("api")).unwrap();
        let mut current = store.get("Deployment", "default", "api").unwrap().unwrap();
        current["status"] = json!({"replicas": 1});
        store.update("Deployment", "default", "api", &current).unwrap();
        let uid = current["metadata"]["uid"].clone();

        let mut desired = deployment("api");
        desired["metadata"]["labels"] = json!({"team": "a"});
        update(&store, &desired, "default").unwrap();
        let after = store.get("Deployment", "default", "api").unwrap().unwrap();
        assert_eq!(after["metadata"]["uid"], uid);
        assert_eq!(after["metadata"]["generation"], 1, "metadata-only change");
        assert_eq!(after["status"]["replicas"], 1);
        assert_eq!(after["metadata"]["labels"]["team"], "a");

        desired["spec"]["replicas"] = json!(3);
        update(&store, &desired, "default").unwrap();
        let after = store.get("Deployment", "default", "api").unwrap().unwrap();
        assert_eq!(after["metadata"]["generation"], 2);
        assert_eq!(after["spec"]["replicas"], 3);
    }

    #[test]
    fn rejected_updates_change_nothing() {
        let store = store();
        create_client(&store, &deployment("api")).unwrap();
        let before = store.get("Deployment", "default", "api").unwrap().unwrap();
        let mut desired = deployment("api");
        desired["spec"]["template"]["spec"]["containers"][0]["image"] = json!("");
        assert_eq!(
            update(&store, &desired, "default").unwrap_err().reason(),
            Some("Invalid")
        );
        assert_eq!(store.get("Deployment", "default", "api").unwrap().unwrap(), before);

        let err = update(&store, &deployment("ghost"), "default").unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error from server (NotFound): deployments.apps \"ghost\" not found"
        );
    }

    #[test]
    fn every_kind_roundtrips_a_minimal_manifest() {
        for info in kinds() {
            let manifest = json!({"apiVersion": info.api_version, "kind": info.kind, "metadata": {"name": "x"}});
            let decoded = (info.roundtrip)(&manifest).unwrap();
            assert_eq!(decoded["kind"], info.kind);
            assert!(first_unknown_field(&manifest, &decoded, "").is_none(), "{}", info.kind);
        }
    }
}
