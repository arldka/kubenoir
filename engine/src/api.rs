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
     [$($short:literal),*], namespaced: $ns:literal, generation: $generation:literal) => {
        KindInfo {
            kind: $kind,
            api_version: $av,
            group: $group,
            plural: $plural,
            singular: $singular,
            short_names: &[$($short),*],
            namespaced: $ns,
            has_generation: $generation,
            roundtrip: roundtrip::<$ty>,
        }
    };
}

use k8s_openapi::api::apps::v1 as apps;
use k8s_openapi::api::batch::v1 as batch;
use k8s_openapi::api::core::v1 as core;
use k8s_openapi::api::discovery::v1 as discovery;
use k8s_openapi::api::networking::v1 as networking;

static KINDS: [KindInfo; 14] = [
    kind!(core::ConfigMap, "ConfigMap", "", "v1", "configmaps", "configmap", ["cm"], namespaced: true, generation: false),
    kind!(batch::CronJob, "CronJob", "batch", "batch/v1", "cronjobs", "cronjob", ["cj"], namespaced: true, generation: true),
    kind!(apps::Deployment, "Deployment", "apps", "apps/v1", "deployments", "deployment", ["deploy"], namespaced: true, generation: true),
    kind!(core::Endpoints, "Endpoints", "", "v1", "endpoints", "endpoints", ["ep"], namespaced: true, generation: false),
    kind!(discovery::EndpointSlice, "EndpointSlice", "discovery.k8s.io", "discovery.k8s.io/v1", "endpointslices", "endpointslice", [], namespaced: true, generation: false),
    kind!(core::Event, "Event", "", "v1", "events", "event", ["ev"], namespaced: true, generation: false),
    kind!(batch::Job, "Job", "batch", "batch/v1", "jobs", "job", [], namespaced: true, generation: true),
    kind!(core::Namespace, "Namespace", "", "v1", "namespaces", "namespace", ["ns"], namespaced: false, generation: false),
    kind!(networking::NetworkPolicy, "NetworkPolicy", "networking.k8s.io", "networking.k8s.io/v1", "networkpolicies", "networkpolicy", ["netpol"], namespaced: true, generation: false),
    kind!(core::Node, "Node", "", "v1", "nodes", "node", ["no"], namespaced: false, generation: false),
    kind!(core::Pod, "Pod", "", "v1", "pods", "pod", ["po"], namespaced: true, generation: true),
    kind!(apps::ReplicaSet, "ReplicaSet", "apps", "apps/v1", "replicasets", "replicaset", ["rs"], namespaced: true, generation: true),
    kind!(core::Secret, "Secret", "", "v1", "secrets", "secret", [], namespaced: true, generation: false),
    kind!(core::Service, "Service", "", "v1", "services", "service", ["svc"], namespaced: true, generation: false),
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

/// Kubernetes kinds (lowercase singular names) that exist but that this terminal does not
/// simulate. Anything registered in `KINDS` is simulated and not listed here.
const UNSIMULATED_KINDS: &[&str] = &[
    "clusterrole",
    "clusterrolebinding",
    "componentstatus",
    "customresourcedefinition",
    "daemonset",
    "horizontalpodautoscaler",
    "ingress",
    "ingressclass",
    "lease",
    "limitrange",
    "persistentvolume",
    "persistentvolumeclaim",
    "poddisruptionbudget",
    "podtemplate",
    "priorityclass",
    "replicationcontroller",
    "resourcequota",
    "role",
    "rolebinding",
    "serviceaccount",
    "statefulset",
    "storageclass",
    "volumeattachment",
];

/// Short and plural names of those kinds, as kubectl accepts them.
const UNSIMULATED_NAMES: &[&str] = &[
    "all",
    "clusterroles",
    "clusterrolebindings",
    "componentstatuses",
    "cs",
    "crd",
    "crds",
    "customresourcedefinitions",
    "daemonsets",
    "ds",
    "hpa",
    "horizontalpodautoscalers",
    "ing",
    "ingresses",
    "ingressclasses",
    "leases",
    "limitranges",
    "limits",
    "pc",
    "pdb",
    "persistentvolumes",
    "persistentvolumeclaims",
    "poddisruptionbudgets",
    "podtemplates",
    "pv",
    "pvc",
    "priorityclasses",
    "quota",
    "rc",
    "replicationcontrollers",
    "resourcequotas",
    "rolebindings",
    "roles",
    "sa",
    "serviceaccounts",
    "sc",
    "statefulsets",
    "sts",
    "storageclasses",
    "volumeattachments",
];

/// Whether a kubectl resource argument names a real Kubernetes resource this terminal does not
/// simulate (as opposed to something that is not a resource at all).
pub fn is_unsimulated_resource(arg: &str) -> bool {
    let name = arg.to_ascii_lowercase();
    let name = name.split('.').next().unwrap_or_default();
    kind_for_arg(arg).is_none() && (UNSIMULATED_NAMES.contains(&name) || UNSIMULATED_KINDS.contains(&name))
}

/// The error for a manifest whose kind is not registered: "not available here" if real
/// Kubernetes has it, else the missing-CRD error kubectl gives.
pub fn kind_error(kind: &str, api_version: &str) -> ApiError {
    if UNSIMULATED_KINDS.contains(&kind.to_ascii_lowercase().as_str()) {
        ApiError::NotSimulated { kind: kind.to_string() }
    } else {
        ApiError::NoKindMatch {
            kind: kind.to_string(),
            api_version: api_version.to_string(),
        }
    }
}

/// The registry entry for a kind name (`Pod`).
pub fn info(kind: &str) -> Option<&'static KindInfo> {
    KINDS.iter().find(|k| k.kind == kind)
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
    /// A real Kubernetes kind that this terminal does not simulate.
    NotSimulated {
        kind: String,
    },
    /// New content in a namespace that is being deleted.
    Terminating {
        namespace: String,
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
    /// A strategic merge patch that cannot be computed or applied (upstream's error text).
    Patch(String),
    Store(StoreError),
}

impl ApiError {
    /// Reason in `Error from server (<reason>)`; `None` for errors kubectl raises itself.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            ApiError::MissingTypeMeta { .. }
            | ApiError::NoKindMatch { .. }
            | ApiError::NotSimulated { .. }
            | ApiError::Store(_) => None,
            ApiError::Terminating { .. } => Some("Forbidden"),
            ApiError::BadRequest { .. } => Some("BadRequest"),
            ApiError::Invalid { .. } => Some("Invalid"),
            ApiError::NotFound { .. } => Some("NotFound"),
            ApiError::AlreadyExists { .. } => Some("AlreadyExists"),
            ApiError::Patch(message) if crate::smp::is_bad_request(message) => Some("BadRequest"),
            ApiError::Patch(_) => None,
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
            ApiError::NotSimulated { kind } => format!("the {kind} kind is not available in this terminal"),
            ApiError::Terminating { namespace } => {
                format!("unable to create new content in namespace {namespace} because it is being terminated")
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
            ApiError::Patch(message) => message.clone(),
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
    let meta = object["metadata"].as_object_mut().expect("object");
    meta.entry("creationTimestamp")
        .or_insert_with(|| Value::String(clock::format(now)));
    if info.has_generation {
        meta.entry("generation").or_insert(json!(1));
    }
    // The uid the store would give it, now: validation (a Job's selector) needs it.
    if meta.get("uid").and_then(Value::as_str).is_none_or(str::is_empty) {
        meta.insert("uid".into(), Value::String(names::uid(store.seed(), store.next_id())));
    }
    // What the API server does, in its order: decode (defaults), prepare, validate.
    apply_defaults(info, &name, &mut object);
    prepare(info, &mut object);
    invalid(info, &name, crate::validation::validate_create(info.kind, &object))?;

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
    apply_defaults(info, &name, &mut object);
    // Identity is the stored object's; a client that sends no resourceVersion updates
    // unconditionally (the server fills in the current one).
    let meta = object["metadata"].as_object_mut().expect("object");
    for key in ["uid", "creationTimestamp", "generation"] {
        match current["metadata"].get(key) {
            Some(value) => meta.insert(key.into(), value.clone()),
            None => meta.remove(key),
        };
    }
    if meta
        .get("resourceVersion")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        if let Some(version) = current["metadata"].get("resourceVersion") {
            meta.insert("resourceVersion".into(), version.clone());
        }
    }
    prepare(info, &mut object);
    invalid(
        info,
        &name,
        crate::validation::validate_update(info.kind, &object, &current),
    )?;

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
    if !info.namespaced {
        return Ok(());
    }
    match store.get("Namespace", "", namespace)? {
        None => Err(ApiError::NotFound {
            resource: "namespaces".into(),
            name: namespace.to_string(),
        }),
        Some(ns) if ns["metadata"].get("deletionTimestamp").is_some() => Err(ApiError::Terminating {
            namespace: namespace.to_string(),
        }),
        Some(_) => Ok(()),
    }
}

/// Resolve the kind and decode strictly: the object as the server keeps it, with
/// `metadata.namespace` set for namespaced kinds and removed for cluster-scoped ones.
pub(crate) fn admit(manifest: &Value, namespace: &str) -> Result<(&'static KindInfo, Value), ApiError> {
    let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or_default();
    let api_version = manifest.get("apiVersion").and_then(Value::as_str).unwrap_or_default();
    if kind.is_empty() || api_version.is_empty() {
        return Err(ApiError::MissingTypeMeta {
            kind_missing: kind.is_empty(),
            api_version_missing: api_version.is_empty(),
        });
    }
    let info = kind_for(kind, api_version).ok_or_else(|| kind_error(kind, api_version))?;
    let bad_request = |reason: String| ApiError::BadRequest {
        kind: info.kind,
        api_version: info.api_version,
        reason,
    };

    let mut object = (info.roundtrip)(manifest).map_err(bad_request)?;
    if let Some(field) = first_unknown_field(manifest, &object, "") {
        return Err(bad_request(format!("strict decoding error: unknown field \"{field}\"")));
    }
    // The typed round trip fills required fields the manifest left out (`selector: {}`); Go
    // leaves them unset, which validation reports ("spec.selector: Required value").
    prune_to(manifest, &mut object);
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

/// Drop from `object` what `manifest` does not have (recursively, list items by position).
fn prune_to(manifest: &Value, object: &mut Value) {
    match (manifest, object) {
        (Value::Object(m), Value::Object(o)) => {
            o.retain(|k, _| m.contains_key(k));
            for (k, v) in o.iter_mut() {
                prune_to(&m[k], v);
            }
        }
        (Value::Array(m), Value::Array(o)) => {
            for (mi, oi) in m.iter().zip(o.iter_mut()) {
                prune_to(mi, oi);
            }
        }
        _ => {}
    }
}

/// An `Invalid` error for these causes, if there are any.
fn invalid(info: &KindInfo, name: &str, causes: Vec<String>) -> Result<(), ApiError> {
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

/// The registry strategies' `PrepareForCreate`/`PrepareForUpdate` steps validation depends on:
/// a Job's generated selector and labels, and a Service's `clusterIPs` from its `clusterIP`
/// (the Service REST handler's normalization).
fn prepare(info: &KindInfo, object: &mut Value) {
    match info.kind {
        "Job" => crate::validation::generate_job_selector(object),
        "Service" => {
            let ip = object["spec"]["clusterIP"].as_str().unwrap_or_default().to_string();
            let ips_empty = object["spec"]["clusterIPs"].as_array().is_none_or(Vec::is_empty);
            if !ip.is_empty() && ips_empty {
                object["spec"]["clusterIPs"] = json!([ip]);
            }
        }
        _ => {}
    }
}

/// What kube-apiserver says when a pod update touches anything but the few mutable fields
/// (upstream also appends a diff of the internal pod specs).
pub const POD_UPDATE_FORBIDDEN: &str = "spec: Forbidden: pod updates may not change fields other than `spec.containers[*].image`,`spec.initContainers[*].image`,`spec.activeDeadlineSeconds`,`spec.tolerations` (only additions to existing tolerations),`spec.terminationGracePeriodSeconds` (allow it to be set to 1 if it was previously negative)";

// ---- defaulting -------------------------------------------------------------------------------

fn set_default(fields: &mut Map<String, Value>, key: &str, value: Value) {
    fields.entry(key).or_insert(value);
}

/// What the API server does to an object before validating and storing it: the upstream
/// defaulters (`crate::defaults`), then the rest of the server's own changes that are not
/// defaulting: Secret `stringData` folded into `data` (conversion), a new Namespace's
/// `kubernetes` finalizer (create strategy), and a simulated Node's kubelet status.
fn apply_defaults(info: &KindInfo, name: &str, object: &mut Value) {
    let _ = name;
    crate::defaults::set_object_defaults(info.kind, object);
    match info.kind {
        "Namespace" => {
            set_default(obj(&mut object["spec"]), "finalizers", json!(["kubernetes"]));
        }
        "Secret" => {
            // `stringData` is write-only: the API server stores it base64 encoded in `data`, where
            // it wins over an existing key of the same name.
            let strings = object.as_object_mut().and_then(|o| o.remove("stringData"));
            if let Some(Value::Object(strings)) = strings {
                let data = obj(obj(object).entry("data").or_insert_with(|| json!({})));
                for (key, value) in strings {
                    if let Some(text) = value.as_str() {
                        data.insert(key, json!(crate::shell::base64_encode(text.as_bytes())));
                    }
                }
            }
        }
        "Node" => {
            // What a kubelet reports for a healthy node; authored status wins.
            let status = obj(&mut object["status"]);
            set_default(
                status,
                "conditions",
                json!([{"type": "Ready", "status": "True", "reason": "KubeletReady",
                        "message": "kubelet is posting ready status"}]),
            );
            set_default(
                obj(status.entry("nodeInfo").or_insert_with(|| json!({}))),
                "kubeletVersion",
                json!("v1.36.0"),
            );
        }
        _ => {}
    }
    // Stored the way the API server stores it: encoded from its Go type.
    *object = crate::gojson::encode_object(object);
}

/// `value` as an object, turning it into an empty one first if it is anything else.
fn obj(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().expect("just made an object")
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
        // The client's status is dropped; what is stored is the Go-encoded empty status.
        assert_eq!(m["status"], json!({}));
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
        assert!(
            err.message()
                .contains("metadata.name: Invalid value: \"a.b\": must not contain dots"),
            "{err}"
        );
    }

    #[test]
    fn workload_selector_must_match_template_labels() {
        let store = store();
        let mut manifest = deployment("api");
        manifest["spec"]["template"]["metadata"]["labels"] = json!({"app": "other"});
        let err = create_client(&store, &manifest).unwrap_err();
        assert_eq!(
            err.message(),
            "The Deployment \"api\" is invalid: spec.template.metadata.labels: Invalid value: {\"app\":\"other\"}: `selector` does not match template `labels`"
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
