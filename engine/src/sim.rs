//! The simulation engine proper, in plain Rust (no wasm-bindgen) so it is natively testable.
//!
//! The engine owns the clock. Time only moves through `advance`, so the whole simulation is a
//! pure function of the scenario plus the ordered `eval` / `advance` calls made on it.

use serde::Deserialize;

use crate::apps::AppSet;
use crate::clock;
use crate::complete::{complete, Completion};
use crate::controllers::{self, config::ClusterConfig, Env};
use crate::judge::{Judge, Status};
use crate::manifests::{normalize_path, Files};
use crate::scenario::{load, ScenarioError};
use crate::shell;
use crate::store::ClusterStore;

/// Largest single `advance`, in simulated seconds (one day).
pub const MAX_ADVANCE_SECS: u32 = 86_400;

/// A simulated cluster: seeded state store plus the scenario's notion of "now".
pub struct Engine {
    store: ClusterStore,
    config: ClusterConfig,
    judge: Judge,
    /// Simulated time, epoch seconds.
    now: i64,
    /// Text files the player's commands can read (`kubectl apply -f`).
    files: Files,
}

/// One recorded player input. Replaying a list of these on a fresh engine reproduces the
/// session exactly (hint requests are recorded by the UI but do not touch the cluster).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Step {
    Eval { command: String },
    Advance { seconds: u32 },
    Hint {},
}

impl Engine {
    /// Build a cluster from scenario JSON and the manifest files it references (by reference;
    /// see `scenario::required_files`), running the given apps (every app the scenario's images
    /// reference must be in `apps`; see `scenario::required_apps`).
    pub fn from_scenario(scenario_json: &str, manifests: &Files, apps: AppSet) -> Result<Self, ScenarioError> {
        let cluster = load(scenario_json, manifests, apps)?;
        let mut engine = Engine {
            store: cluster.store,
            config: cluster.config,
            judge: Judge::new(cluster.rules),
            now: clock::secs(&cluster.started_at),
            files: cluster.files,
        };
        // Let the control plane notice the seeded objects, then live through the history the
        // scenario asks for so the player starts at `now` with a cluster that has a past.
        engine.run_until(clock::secs(&cluster.now));
        // Judging begins here: the backstory does not count.
        engine.judge.start(engine.now);
        engine.judge.observe(&engine.store, engine.now);
        Ok(engine)
    }

    /// Run a recorded session on a fresh engine and report how it ended: the stateless
    /// verification primitive. The same scenario and steps always give the same status.
    pub fn replay(
        scenario_json: &str,
        manifests: &Files,
        steps: &[Step],
        apps: AppSet,
    ) -> Result<Status, ScenarioError> {
        let mut engine = Engine::from_scenario(scenario_json, manifests, apps)?;
        for step in steps {
            match step {
                Step::Eval { command } => {
                    engine.eval(command);
                }
                Step::Advance { seconds } => engine.advance(*seconds),
                Step::Hint {} => {}
            }
        }
        Ok(engine.status())
    }

    /// How the case stands: solved or not, findings so far, and (once closed) the report.
    pub fn status(&self) -> Status {
        self.judge.status(self.now)
    }

    fn reconcile(&self) -> bool {
        controllers::reconcile(&Env {
            store: &self.store,
            config: &self.config,
            now: self.now,
        })
        .is_ok()
    }

    /// Simulated time, epoch seconds.
    #[cfg(test)]
    pub(crate) fn now_secs(&self) -> i64 {
        self.now
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &ClusterStore {
        &self.store
    }

    /// Tab completion for the text left of the cursor.
    pub fn complete(&self, line: &str) -> Completion {
        complete(line, &self.store)
    }

    /// Current simulated time, RFC 3339.
    pub fn now(&self) -> String {
        clock::format(self.now)
    }

    /// Move simulated time forward by `secs` (clamped to [`MAX_ADVANCE_SECS`]), running the
    /// control loops at every instant something is due.
    pub fn advance(&mut self, secs: u32) {
        self.run_until(self.now + i64::from(secs.min(MAX_ADVANCE_SECS)));
    }

    fn run_until(&mut self, target: i64) {
        loop {
            // Control-loop failures indicate a corrupt store; there is no caller to report to
            // mid-simulation, so the cluster simply stops evolving.
            if !self.reconcile() {
                break;
            }
            self.judge.observe(&self.store, self.now);
            if self.now >= target {
                break;
            }
            // The judge also needs the clock to stop when its stability window completes, so the
            // moment of solving is exact however time is advanced.
            let store_wake = self.store.next_wake().ok().flatten();
            let next_wake = [store_wake, self.judge.next_wake()]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(target);
            // Reconcile leaves nothing due at `now`, so the next wake is strictly later; the
            // `+ 1` floor only guarantees forward progress if a loop ever breaks that rule.
            self.now = next_wake.min(target).max(self.now + 1);
        }
    }

    /// Create or overwrite a text file the terminal's commands can read.
    pub fn write_file(&mut self, path: &str, contents: &str) {
        self.files.insert(normalize_path(path), contents.to_string());
    }

    /// Evaluate one terminal line (a pipeline, see `shell`) and return the text to print.
    /// Never panics: parse errors come back as kubectl-style error strings.
    pub fn eval(&mut self, line: &str) -> String {
        let env = Env {
            store: &self.store,
            config: &self.config,
            now: self.now,
        };
        let ran = shell::run(line, &env, &mut self.files);
        for executed in &ran.executed {
            self.judge
                .record_command(&executed.command, &executed.touched, &executed.output, self.now);
        }
        // Commands may have changed desired state; let the cluster react right away.
        self.reconcile();
        self.judge.observe(&self.store, self.now);
        ran.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO: &str = r#"{"id":"t","cluster":{"now":"2026-10-27T18:30:00Z","pods":[]}}"#;

    fn engine() -> Engine {
        crate::scenario::inline::engine(SCENARIO)
    }

    #[test]
    fn now_starts_at_scenario_time_and_advances() {
        let mut e = engine();
        assert_eq!(e.now(), "2026-10-27T18:30:00Z");
        e.advance(90);
        assert_eq!(e.now(), "2026-10-27T18:31:30Z");
        e.advance(0);
        assert_eq!(e.now(), "2026-10-27T18:31:30Z");
    }

    #[test]
    fn advance_is_clamped_to_one_day() {
        let mut e = engine();
        e.advance(u32::MAX);
        assert_eq!(e.now(), "2026-10-28T18:30:00Z");
    }

    #[test]
    fn one_big_advance_equals_many_small_ones() {
        let mut big = engine();
        big.advance(60);
        let mut small = engine();
        for _ in 0..60 {
            small.advance(1);
        }
        assert_eq!(big.now(), small.now());
    }

    #[test]
    fn eval_still_answers_commands() {
        let mut e = engine();
        assert_eq!(e.eval("kubectl get pods"), "No resources found in default namespace.");
    }
}
