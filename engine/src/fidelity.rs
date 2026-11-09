//! The engine functions `tests/fidelity.rs` compares with upstream Kubernetes (the expected
//! outputs `oracle/` writes; see `docs/fidelity.md`). Inputs and outputs are plain text and
//! JSON values so the test harness needs none of the engine's internals.

use serde_json::Value;

fn parse(what: &str, text: &str) -> Result<Value, String> {
    serde_yaml_ng::from_str(text).map_err(|e| format!("bad {what}: {e}"))
}

/// `live` patched with `patch` as a strategic merge patch (`kubectl patch`'s default type).
/// `Err` is the patch error's text.
pub fn strategic_merge(live: &str, patch: &str) -> Result<Value, String> {
    let live = parse("live object", live)?;
    let patch = parse("patch", patch)?;
    crate::smp::strategic_merge_patch(&live, &patch)
}

/// `kubectl apply -f` of `manifest` onto `live` (the stored object): `{patch, result}`, the
/// patch kubectl sends (empty: `unchanged`, nothing sent) and the object after the API server
/// merged it. A namespaced manifest without a namespace goes to `default`.
pub fn apply(live: &str, manifest: &str) -> Result<Value, String> {
    let live = parse("live object", live)?;
    let mut config = parse("manifest", manifest)?;
    let kind = config["kind"].as_str().unwrap_or_default().to_string();
    if !matches!(kind.as_str(), "Namespace" | "Node") && config["metadata"]["namespace"].is_null() {
        config["metadata"]["namespace"] = Value::String("default".into());
    }
    let modified = crate::manifests::modified_configuration(&config);
    let original = live["metadata"]["annotations"][crate::manifests::LAST_APPLIED].as_str();
    let patch = crate::smp::create_three_way_merge_patch(original, &modified, &live)?;
    let result = if patch.as_object().is_some_and(|p| p.is_empty()) {
        live.clone()
    } else {
        crate::smp::strategic_merge_patch(&live, &patch)?
    };
    Ok(serde_json::json!({ "patch": patch, "result": result }))
}

/// `manifest` decoded the way the API server's write path decodes it, defaulted with the
/// upstream defaulters only (no create strategy, no admission) and encoded as upstream encodes
/// it; `status` left out.
pub fn defaults(manifest: &str) -> Result<Value, String> {
    let manifest = parse("manifest", manifest)?;
    let (info, mut object) = crate::api::admit(&manifest, "default").map_err(|e| e.message())?;
    crate::defaults::set_object_defaults(info.kind, &mut object);
    let mut object = crate::gojson::encode_object(&object);
    if let Some(fields) = object.as_object_mut() {
        fields.remove("status");
    }
    Ok(object)
}

/// A fixed uid, as the oracle gives every object (a Job's generated selector depends on it).
const FIDELITY_UID: &str = "6f0f5c2e-0000-4000-8000-000000000001";

/// `manifest` created (or, with `old`, updated from that stored object) through the write
/// path's decode, defaults, prepare and validation. `Err` is the causes, one per line, as kubectl
/// prints them; `Ok` is `valid: true`.
pub fn validate(manifest: &str, old: Option<&str>) -> Result<Value, String> {
    let stored = |text: &str, created: bool| -> Result<(&'static str, Value), String> {
        let manifest = parse("manifest", text)?;
        let (info, mut object) = crate::api::admit(&manifest, "default").map_err(|e| e.message())?;
        crate::defaults::set_object_defaults(info.kind, &mut object);
        let mut object = crate::gojson::encode_object(&object);
        object["metadata"]["uid"] = Value::String(FIDELITY_UID.into());
        object["metadata"]["resourceVersion"] = Value::String(if created { "" } else { "1" }.into());
        if info.kind == "Job" && (created || old.is_some()) {
            crate::validation::generate_job_selector(&mut object);
        }
        if info.kind == "Service" {
            let ip = object["spec"]["clusterIP"].as_str().unwrap_or_default().to_string();
            if !ip.is_empty() && object["spec"]["clusterIPs"].as_array().is_none_or(Vec::is_empty) {
                object["spec"]["clusterIPs"] = serde_json::json!([ip]);
            }
        }
        Ok((info.kind, object))
    };
    let causes = match old {
        None => {
            let (kind, object) = stored(manifest, true)?;
            crate::validation::validate_create(kind, &object)
        }
        Some(old) => {
            let (kind, old) = stored(old, true)?;
            let (_, mut new) = stored(manifest, false)?;
            if kind == "Job" {
                // Only the stored object went through the create path.
                new["spec"]["selector"] = old["spec"]["selector"].clone();
            }
            crate::validation::validate_update(kind, &new, &old)
        }
    };
    if causes.is_empty() {
        Ok(serde_json::json!({ "valid": true }))
    } else {
        Err(causes.join("\n"))
    }
}
