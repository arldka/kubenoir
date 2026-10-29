//! The apps an engine can run, keyed by reference (`name@version`, like an image tag). The engine
//! is given them at start: the browser fetches them from an app registry (see `docs/registry.md`),
//! native tests and tools use the locally built ones (`bundled`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use wasmi::{Config, Engine, Module};

/// Compiled apps, all on one shared fuel-metered wasmi engine. Cloning is cheap.
#[derive(Clone, Default)]
pub struct AppSet {
    modules: BTreeMap<String, Module>,
}

impl fmt::Debug for AppSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.modules.keys()).finish()
    }
}

fn shared_engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::default();
        config.consume_fuel(true);
        Engine::new(&config)
    })
}

/// Is `reference` of the form `name@version`?
pub fn is_reference(reference: &str) -> bool {
    reference
        .split_once('@')
        .is_some_and(|(name, version)| !name.is_empty() && !version.is_empty() && !version.contains('@'))
}

impl AppSet {
    /// Compile and add an app. Fails if the reference is malformed or the bytes are not a valid
    /// WebAssembly module.
    pub fn insert(&mut self, reference: &str, wasm: &[u8]) -> Result<(), String> {
        if !is_reference(reference) {
            return Err(format!("app reference {reference:?} must be name@version"));
        }
        let module = Module::new(shared_engine(), wasm).map_err(|e| format!("app {reference} is not valid: {e}"))?;
        self.modules.insert(reference.to_string(), module);
        Ok(())
    }

    pub fn get(&self, reference: &str) -> Option<&Module> {
        self.modules.get(reference)
    }

    pub fn contains(&self, reference: &str) -> bool {
        self.modules.contains_key(reference)
    }

    /// The apps built from `../apps` by `build.rs`, under their `Cargo.toml` versions. Compiled
    /// once per process.
    #[cfg(feature = "bundled-apps")]
    pub fn bundled() -> AppSet {
        const BUNDLED_APPS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/bundled_apps.rs"));
        static BUNDLED: OnceLock<AppSet> = OnceLock::new();
        BUNDLED
            .get_or_init(|| {
                let mut set = AppSet::default();
                for (reference, wasm) in BUNDLED_APPS {
                    set.insert(reference, wasm).expect("bundled app compiles");
                }
                set
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_need_a_name_and_a_version() {
        assert!(is_reference("db-client@0.2.0"));
        for bad in ["db-client", "@1", "db-client@", "a@b@c", ""] {
            assert!(!is_reference(bad), "{bad}");
        }
    }

    #[test]
    fn insert_rejects_bad_references_and_bytes() {
        let mut set = AppSet::default();
        assert!(set.insert("x", b"\0asm").unwrap_err().contains("name@version"));
        assert!(set
            .insert("x@1", b"not wasm")
            .unwrap_err()
            .contains("app x@1 is not valid"));
        assert!(!set.contains("x@1"));
    }

    #[test]
    fn bundled_apps_are_versioned() {
        let set = AppSet::bundled();
        for name in ["db-client", "nginx", "script"] {
            assert!(
                set.modules.keys().any(|r| r.starts_with(&format!("{name}@"))),
                "{set:?}"
            );
        }
    }
}
