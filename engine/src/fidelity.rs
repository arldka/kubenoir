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
    let mut live = parse("live object", live)?;
    let patch = parse("patch", patch)?;
    crate::patch::strategic_merge(&mut live, &patch);
    Ok(live)
}
