//! With the `bundled-apps` feature (on by default; native tests and tools), builds the template
//! apps in `../apps` (a separate cargo workspace) for wasm32-wasip1 and generates
//! `$OUT_DIR/bundled_apps.rs`: a `&[("name@version", bytes)]` table that `AppSet::bundled`
//! embeds. The browser build turns the feature off and gets its apps from an
//! app registry instead.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let apps_dir = manifest_dir.join("../apps");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // Watch the sources, not `apps/target` (which `make registry` writes to).
    for entry in fs::read_dir(&apps_dir).unwrap().flatten() {
        if entry.file_name() != "target" {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    if env::var_os("CARGO_FEATURE_BUNDLED_APPS").is_none() {
        return;
    }

    let target_dir = out_dir.join("apps-target");
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .current_dir(&apps_dir)
        .args(["build", "--release", "--target", "wasm32-wasip1", "--target-dir"])
        .arg(&target_dir);
    // The outer build's flags and target are for the engine, not for the apps.
    for var in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_MAKEFLAGS",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        build.env_remove(var);
    }
    let status = build.status().expect("failed to run cargo to build the template apps");
    assert!(
        status.success(),
        "building the template apps failed (needs the wasm32-wasip1 target: `rustup target add wasm32-wasip1`)"
    );

    let built = target_dir.join("wasm32-wasip1/release");
    let mut table = String::from("&[\n");
    for (name, version) in apps(&apps_dir) {
        let wasm = built.join(format!("{name}.wasm"));
        assert!(wasm.exists(), "missing built app {}", wasm.display());
        table.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            format!("{name}@{version}"),
            wasm
        ));
    }
    table.push_str("]\n");
    fs::write(out_dir.join("bundled_apps.rs"), table).unwrap();
}

/// `(name, version)` of every app: each workspace member with a `src/main.rs`.
fn apps(apps_dir: &Path) -> Vec<(String, String)> {
    let mut apps: Vec<_> = fs::read_dir(apps_dir)
        .unwrap()
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            if !dir.join("src/main.rs").exists() {
                return None;
            }
            let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
            Some((field(&manifest, "name")?, field(&manifest, "version")?))
        })
        .collect();
    apps.sort();
    apps
}

/// A `key = "value"` line from a manifest's `[package]` table (the first such line).
fn field(manifest: &str, key: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}
