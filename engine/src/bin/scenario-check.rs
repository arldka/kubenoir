//! `scenario-check <scenario.json>...`: the scenario author's quick loop, without a browser.
//!
//! Loads each scenario with the locally built apps (so every app it references must exist in
//! `apps/` at that version) and reports what is wrong. If a `solution.json` sits next to the
//! scenario (recorded steps, the format `replay` takes), plays it and checks that it closes the
//! case. Exits non-zero if any scenario fails.

use std::path::Path;
use std::process::ExitCode;

use engine::{AppSet, Engine, Step};

fn check(path: &Path) -> Result<String, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))?;
    Engine::from_scenario(&json, AppSet::bundled()).map_err(|e| e.to_string())?;

    let solution = path.with_file_name("solution.json");
    if !solution.exists() {
        return Ok("loads (no solution.json to play)".into());
    }
    let steps = std::fs::read_to_string(&solution).map_err(|e| format!("cannot read solution.json: {e}"))?;
    let steps: Vec<Step> = serde_json::from_str(&steps).map_err(|e| format!("invalid solution.json: {e}"))?;
    let status = Engine::replay(&json, &steps, AppSet::bundled()).map_err(|e| e.to_string())?;
    match (status.solved, status.solved_at_seconds) {
        (true, Some(at)) => Ok(format!(
            "loads, solution.json solves it at {at}s ({})",
            status.outcome.unwrap_or("closed")
        )),
        _ => Err("solution.json does not solve the case".into()),
    }
}

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: scenario-check <scenario.json>...");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for path in &paths {
        match check(Path::new(path)) {
            Ok(summary) => println!("ok    {path}: {summary}"),
            Err(error) => {
                failed = true;
                println!("FAIL  {path}: {error}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
