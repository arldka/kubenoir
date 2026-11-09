//! Upstream fidelity (V33): every case under `tests/fidelity/<area>/<case>/` holds inputs and the
//! output upstream Kubernetes produced for them (`expected.yaml` or `expected.err`, written by
//! `make oracle-goldens` from `oracle/`). The engine must produce the same. Values are compared
//! structurally (key order and number spelling aside); error texts byte for byte.
//!
//! All cases of an area run before the test fails, and the failure lists every mismatch.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity")
}

/// The case directories of an area, sorted by name.
fn cases(area: &str) -> Vec<PathBuf> {
    let dir = root().join(area);
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("readable dir").path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no cases in {}", dir.display());
    cases
}

fn read(case: &Path, name: &str) -> String {
    let path = case.join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// What upstream produced for a case: a value or an error text.
enum Expected {
    Value(Value),
    Err(String),
}

fn expected(case: &Path) -> Expected {
    let yaml = case.join("expected.yaml");
    let err = case.join("expected.err");
    match (yaml.exists(), err.exists()) {
        (true, false) => {
            Expected::Value(serde_yaml_ng::from_str(&read(case, "expected.yaml")).expect("expected.yaml parses"))
        }
        (false, true) => Expected::Err(read(case, "expected.err").trim_end_matches('\n').to_string()),
        _ => panic!(
            "{}: needs exactly one of expected.yaml / expected.err; run `make oracle-goldens`",
            case.display()
        ),
    }
}

/// Structural equality; numbers by value, so upstream's `1` (a float64 in Go) equals the
/// engine's `1`.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn yaml(value: &Value) -> String {
    serde_yaml_ng::to_string(value).expect("serializable")
}

/// A mismatch report for one case, or `None` when the engine agrees with upstream.
fn check(case: &Path, got: Result<Value, String>) -> Option<String> {
    let name = case.strip_prefix(root()).unwrap_or(case).display();
    match (expected(case), got) {
        (Expected::Value(want), Ok(got)) if same(&want, &got) => None,
        (Expected::Err(want), Err(got)) if want == got.trim_end_matches('\n') => None,
        (Expected::Value(want), Ok(got)) => Some(format!(
            "{name}\n--- upstream\n{}--- engine\n{}",
            yaml(&want),
            yaml(&got)
        )),
        (Expected::Value(want), Err(got)) => Some(format!(
            "{name}\n--- upstream\n{}--- engine error\n{got}\n",
            yaml(&want)
        )),
        (Expected::Err(want), Ok(got)) => Some(format!(
            "{name}\n--- upstream error\n{want}\n--- engine\n{}",
            yaml(&got)
        )),
        (Expected::Err(want), Err(got)) => {
            Some(format!("{name}\n--- upstream error\n{want}\n--- engine error\n{got}\n"))
        }
    }
}

fn run_area(area: &str, engine: impl Fn(&Path) -> Result<Value, String>) {
    let failures: Vec<String> = cases(area)
        .iter()
        .filter_map(|case| check(case, engine(case)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} {area} case(s) differ from upstream:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn strategic_merge_patch_matches_upstream() {
    run_area("patch", |case| {
        engine::fidelity::strategic_merge(&read(case, "live.yaml"), &read(case, "patch.yaml"))
    });
}

#[test]
fn apply_matches_upstream() {
    run_area("apply", |case| {
        engine::fidelity::apply(&read(case, "live.yaml"), &read(case, "manifest.yaml"))
    });
}
