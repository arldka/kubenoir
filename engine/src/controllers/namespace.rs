//! The namespace controller: a namespace that is being deleted is emptied (pods get their grace
//! period) and then removed.

use super::{pod_identity, Env};
use crate::api;
use crate::mutate::delete_object;
use crate::store::StoreError;

/// Owners go before what they own, so nothing is recreated while the namespace drains.
const FIRST: [&str; 2] = ["Deployment", "ReplicaSet"];

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut changed = false;
    for namespace in env.store.list("Namespace", "")? {
        if namespace["metadata"].get("deletionTimestamp").is_none() {
            continue;
        }
        let name = namespace["metadata"]["name"].as_str().unwrap_or_default().to_string();
        let kinds = FIRST.iter().copied().chain(
            api::kinds()
                .iter()
                .filter(|k| k.namespaced && !FIRST.contains(&k.kind))
                .map(|k| k.kind),
        );
        let mut remaining = false;
        for kind in kinds {
            for object in env.store.list(kind, &name)? {
                remaining = true;
                let object_name = object["metadata"]["name"].as_str().unwrap_or_default();
                // A pod already on its way out is waited for, not deleted again.
                if kind == "Pod" && object["metadata"].get("deletionTimestamp").is_some() {
                    continue;
                }
                let _ = pod_identity;
                delete_object(env, kind, &name, object_name, false)?;
                changed = true;
            }
        }
        if !remaining {
            env.store.delete("Namespace", "", &name)?;
            changed = true;
        }
    }
    Ok(changed)
}
