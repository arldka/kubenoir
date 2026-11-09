//! Validation, ported from upstream `k8s.io/kubernetes/pkg/apis/*/validation` (v1.36) and the
//! apimachinery validators they use, run the way the API server runs them: on the defaulted
//! object, through the kind's registry strategy (`Validate` / `ValidateUpdate`), with the field
//! paths, order and message texts upstream produces (SPEC V37, C8). `tests/fidelity.rs` checks
//! the port against upstream itself.
//!
//! Objects are JSON (as stored, `crate::gojson`); a Go zero value is a missing or `null` field.
//! Upstream iterates Go maps in random order where it validates map entries (labels,
//! annotations, resources, data keys); the port goes in sorted key order, which is one of the
//! orders upstream can produce.
//!
//! Not ported (accepted unchecked): pod affinity, topology spread, DNS config, host aliases,
//! scheduling gates, resource claims, Windows options, seccomp/AppArmor fields, pod-level
//! resources, the volume sources the game cannot mount (cloud disks, iSCSI, RBD, ...), Node and
//! Event specifics. Messages that print internal Go structs (the pod update diff, a Job
//! template's immutability) are only partly reproduced.

mod apps;
mod batch;
mod core;
mod field;
mod gojsonerr;
mod meta;
mod names;
mod networking;

use serde_json::{Map, Value};

pub use batch::generate_selector as generate_job_selector;
pub use field::FieldError;

/// `rest.BeforeCreate`: the strategy's validation of a new object.
pub fn validate_create(kind: &str, object: &Value) -> Vec<String> {
    let errs = match kind {
        "Pod" => core::validate_pod_create(object),
        "Service" => core::validate_service_create(object),
        "ConfigMap" => core::validate_config_map(object),
        "Secret" => core::validate_secret(object),
        "Namespace" => core::validate_namespace(object),
        "Endpoints" | "Event" | "Node" | "EndpointSlice" => core::validate_simple(kind, object),
        "Deployment" => apps::validate_deployment(object),
        "ReplicaSet" => apps::validate_replica_set(object),
        "Job" => batch::validate_job(object),
        "CronJob" => batch::validate_cron_job_create(object),
        "NetworkPolicy" => networking::validate_network_policy(object),
        _ => Vec::new(),
    };
    errs.iter().map(FieldError::cause).collect()
}

/// `rest.BeforeUpdate`: the strategy's validation of a changed object against the stored one.
pub fn validate_update(kind: &str, new: &Value, old: &Value) -> Vec<String> {
    let errs = match kind {
        "Pod" => core::validate_pod_update(new, old),
        "Service" => core::validate_service_update(new, old),
        "ConfigMap" => core::validate_config_map_update(new, old),
        "Secret" => core::validate_secret_update(new, old),
        "Namespace" => meta::object_meta_update(&new["metadata"], &old["metadata"]),
        "Endpoints" | "Event" | "Node" | "EndpointSlice" => {
            let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
            errs.extend(core::validate_simple(kind, new));
            errs
        }
        "Deployment" => apps::validate_deployment_update(new, old),
        "ReplicaSet" => apps::validate_replica_set_update(new, old),
        "Job" => batch::validate_job_update(new, old),
        "CronJob" => batch::validate_cron_job_update(new, old),
        "NetworkPolicy" => networking::validate_network_policy_update(new, old),
        _ => Vec::new(),
    };
    errs.iter().map(FieldError::cause).collect()
}

// ---- JSON access with Go zero values ----

pub(crate) fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn i64_of(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or_default()
}

/// A pointer-to-number field: `None` when unset.
pub(crate) fn opt_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

pub(crate) fn bool_of(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or_default()
}

pub(crate) fn obj<'a>(v: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    v.get(key).and_then(Value::as_object)
}

pub(crate) fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// A field is set (not missing, not `null`).
pub(crate) fn has(v: &Value, key: &str) -> bool {
    v.get(key).is_some_and(|x| !x.is_null())
}
