//! Loading the repository's scenarios, with the manifest files they reference.

use std::path::{Path, PathBuf};

use engine::{required_files, AppSet, Engine, Files};

fn folder(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenarios").join(id)
}

/// `scenarios/<id>/scenario.json` and the manifests it lists, by reference.
pub fn scenario(id: &str) -> (String, Files) {
    let folder = folder(id);
    let json = std::fs::read_to_string(folder.join("scenario.json")).expect("scenario exists");
    let files = required_files(&json)
        .expect("valid manifest references")
        .into_iter()
        .map(|reference| {
            let text = std::fs::read_to_string(folder.join(&reference)).expect("manifest exists");
            (reference, text)
        })
        .collect();
    (json, files)
}

pub fn engine(id: &str) -> Engine {
    let (json, files) = scenario(id);
    Engine::from_scenario(&json, &files, AppSet::bundled()).expect("scenario loads")
}
