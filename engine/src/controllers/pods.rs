//! Helpers shared by the controllers that create and delete pods.

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use serde_json::{json, Value};

use super::{pod_identity, Env};
use crate::clock;
use crate::store::StoreError;

/// Seconds a deleted pod lingers as `Terminating`. Real clusters default to 30 s; the
/// simulation shortens it so the cluster visibly reacts within a player's attention span.
pub const POD_DELETION_GRACE_SECS: i64 = 2;

/// An `ownerReferences` entry marking `owner` as the managing controller.
pub fn owner_ref(api_version: &str, kind: &str, name: &str, uid: &str) -> Value {
    json!({
        "apiVersion": api_version,
        "kind": kind,
        "name": name,
        "uid": uid,
        "controller": true,
        "blockOwnerDeletion": true,
    })
}

pub fn is_owned_by(meta: &ObjectMeta, uid: &str) -> bool {
    meta.owner_references.iter().flatten().any(|o| o.uid == uid)
}

pub fn is_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
}

/// Delete a pod the way the API server does: pods the kubelet is running get a deletion
/// timestamp and disappear after the grace period; anything else goes away immediately.
/// `immediate` (`--force` / `--grace-period=0`) skips the grace period.
pub fn delete_pod(env: &Env<'_>, pod: &Pod, immediate: bool) -> Result<(), StoreError> {
    if pod.metadata.deletion_timestamp.is_some() && !immediate {
        return Ok(());
    }
    let (namespace, name, _) = pod_identity(pod);
    if immediate || env.store.runtime_for_pod(&namespace, &name)?.is_empty() {
        env.store.delete("Pod", &namespace, &name)?;
        env.store.purge_pod_state(&namespace, &name)?;
        return Ok(());
    }
    let mut terminating = pod.clone();
    terminating.metadata.deletion_timestamp = Some(clock::to_time(env.now + POD_DELETION_GRACE_SECS));
    terminating.metadata.deletion_grace_period_seconds = Some(POD_DELETION_GRACE_SECS);
    env.store
        .update("Pod", &namespace, &name, &serde_json::to_value(&terminating)?)
}
