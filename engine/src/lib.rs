//! Engine entry point. This file is the only WASM boundary; simulation, parsing and state
//! live in plain-Rust modules.

// Not called outside tests until `apply` and the scenario loader use it (SPEC T2, T3).
#[cfg_attr(not(test), allow(dead_code))]
mod api;
mod apps;
mod clock;
mod complete;
mod controllers;
mod describe;
mod events;
mod exec;
mod judge;
mod logs;
mod mutate;
mod names;
mod parser;
mod render;
mod scenario;
mod sim;
mod store;

pub use apps::AppSet;
pub use judge::Status;
pub use scenario::{required_apps, ScenarioError};
pub use sim::{Engine, Step};

use wasm_bindgen::prelude::*;

/// The apps a scenario runs, fetched by the caller from an app registry: `add` each reference
/// `requiredApps` lists, then pass the set to `new Engine`. Owns Rust memory: call `.free()`.
#[wasm_bindgen(js_name = AppSet)]
#[derive(Default)]
pub struct WasmAppSet(AppSet);

#[wasm_bindgen(js_class = AppSet)]
impl WasmAppSet {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmAppSet {
        WasmAppSet::default()
    }

    /// Compile and add one app (`name@version`, the bytes of its `.wasm`). The bytes are copied
    /// out of JS memory; throws if the reference is malformed or the module is invalid.
    // Not named `wasm`: wasm-bindgen's JS glue calls its module instance `wasm`, and a parameter
    // of that name would shadow it.
    pub fn add(&mut self, reference: &str, bytes: &[u8]) -> Result<(), JsError> {
        self.0.insert(reference, bytes).map_err(|e| JsError::new(&e))
    }
}

/// The app references (`name@version`) a scenario needs, to fetch before `new Engine`.
#[wasm_bindgen(js_name = requiredApps)]
pub fn wasm_required_apps(scenario_json: &str) -> Result<Vec<String>, JsError> {
    Ok(required_apps(scenario_json)?)
}

/// JS-facing handle to a simulated cluster. It owns Rust memory in WASM linear memory, so
/// JS callers must call `.free()` when they are done with it.
#[wasm_bindgen(js_name = Engine)]
pub struct WasmEngine(Engine);

#[wasm_bindgen(js_class = Engine)]
impl WasmEngine {
    /// Build a cluster from scenario JSON and the apps it runs. Throws if the scenario is
    /// invalid or an app it needs is missing from `apps`.
    #[wasm_bindgen(constructor)]
    pub fn new(scenario_json: &str, apps: &WasmAppSet) -> Result<WasmEngine, JsError> {
        Ok(WasmEngine(Engine::from_scenario(scenario_json, apps.0.clone())?))
    }

    /// Evaluate a single terminal command line and return the text to print.
    pub fn eval(&mut self, cmd: &str) -> String {
        self.0.eval(cmd)
    }

    /// Move simulated time forward (at most one day per call), running the cluster's control
    /// loops. The frontend calls this from a timer; replaying the same calls replays the cluster.
    pub fn advance(&mut self, seconds: u32) {
        self.0.advance(seconds);
    }

    /// Current simulated time, RFC 3339.
    pub fn now(&self) -> String {
        self.0.now()
    }

    /// How the case stands, as JSON: `solved`, `outcome`, `findings`, and a `report` once closed.
    pub fn status(&self) -> String {
        serde_json::to_string(&self.0.status()).unwrap_or_else(|_| "{}".into())
    }

    /// Tab completion for the text left of the cursor, as JSON:
    /// `{"start": <char offset of the word being completed>, "candidates": [...]}`.
    pub fn complete(&self, line: &str) -> String {
        serde_json::to_string(&self.0.complete(line)).unwrap_or_else(|_| r#"{"start":0,"candidates":[]}"#.into())
    }
}

/// Replay a recorded session (`[{"kind":"eval","command":...},{"kind":"advance","seconds":...}]`)
/// on a fresh engine and return the resulting status as JSON. Stateless: the same scenario and
/// steps always give the same answer, which is what score verification relies on.
#[wasm_bindgen(js_name = replay)]
pub fn replay_session(scenario_json: &str, steps_json: &str, apps: &WasmAppSet) -> Result<String, JsError> {
    let steps: Vec<Step> =
        serde_json::from_str(steps_json).map_err(|e| JsError::new(&format!("invalid steps: {e}")))?;
    let status = Engine::replay(scenario_json, &steps, apps.0.clone())?;
    Ok(serde_json::to_string(&status).unwrap_or_else(|_| "{}".into()))
}
