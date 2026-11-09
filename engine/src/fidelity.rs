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
