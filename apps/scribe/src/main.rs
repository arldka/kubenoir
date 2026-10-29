//! A service that keeps a log file: it appends `entry N` to `SCRIBE_FILE` every
//! `SCRIBE_INTERVAL` seconds (default 10), creating the file if it is missing. If it cannot write
//! it logs why; an app built with `"exitOnError": true` then gives up instead of carrying on.
//! With `SCRIBE_COUNT=N` it stops after N entries and exits cleanly (a batch job's shape).

use std::io::Write;

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Config {
    name: String,
    version: String,
    /// Whether this build of the service dies when it cannot write.
    exit_on_error: bool,
}

fn append(path: &str, line: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| kit::io_reason(&e))?;
    writeln!(file, "{line}").map_err(|e| kit::io_reason(&e))
}

fn main() {
    let config: Config = kit::config();
    let name = if config.name.is_empty() { "scribe" } else { &config.name };
    kit::info(&format!("{name} {} starting (pid 1)", config.version));

    let Ok(path) = std::env::var("SCRIBE_FILE") else {
        kit::fatal("required environment variable SCRIBE_FILE is not set");
    };
    let interval: u64 = std::env::var("SCRIBE_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(10);

    let limit: Option<u32> = std::env::var("SCRIBE_COUNT").ok().and_then(|v| v.parse().ok());
    let mut entry = 1;
    loop {
        match append(&path, &format!("entry {entry}")) {
            Ok(()) => kit::info(&format!("wrote entry {entry} to {path}")),
            Err(reason) => {
                kit::error(&format!("cannot write {path}: {reason}"));
                if config.exit_on_error {
                    kit::fatal("cannot keep the log, exiting");
                }
            }
        }
        if limit.is_some_and(|limit| entry >= limit) {
            kit::info(&format!("wrote {entry} entries, done"));
            return;
        }
        entry += 1;
        kit::sleep(interval);
    }
}
