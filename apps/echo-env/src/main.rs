//! A service that reports what it was started with: for each name in `vars` it logs
//! `NAME=value`, or says the variable is not set, then serves forever. What it reports is the
//! environment of this run, so a changed environment shows up in the next run's log.

use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    name: String,
    version: String,
    #[serde(default)]
    vars: Vec<String>,
}

fn main() {
    let config: Config = kit::config();
    kit::info(&format!("{} {} starting (pid 1)", config.name, config.version));
    kit::sleep(1);
    for var in &config.vars {
        match std::env::var(var) {
            Ok(value) => kit::info(&format!("{var}={value}")),
            Err(_) => kit::warn(&format!("{var} is not set")),
        }
    }
    kit::info("reporting for duty");
    kit::serve_forever();
}
