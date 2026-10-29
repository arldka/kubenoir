//! A service configured by files. At start it reads each file in `WATCH_FILES` (comma separated
//! absolute paths) and logs what it found. An app built with hot reload (`"reload": true` in the
//! image config) then re-reads them every `WATCH_INTERVAL` seconds (default 30) and logs when
//! one changed; one without it keeps what it read at start for good.
//!
//! A file it cannot read is logged with the reason. With `EXIT_ON_ERROR=1` that is fatal.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    name: String,
    version: String,
    /// Whether this build of the service notices changed files while it runs.
    reload: bool,
}

/// What reading a file gave: its text, or why not.
fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| kit::io_reason(&e))
}

/// A file's text on one log line.
fn show(text: &str) -> String {
    text.trim_end().replace('\n', " | ")
}

fn main() {
    let config: Config = kit::config();
    let name = if config.name.is_empty() {
        "watcher"
    } else {
        &config.name
    };
    kit::info(&format!("{name} {} starting (pid 1)", config.version));

    let paths: Vec<String> = std::env::var("WATCH_FILES")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    if paths.is_empty() {
        kit::fatal("required environment variable WATCH_FILES is not set");
    }
    let exit_on_error = matches!(std::env::var("EXIT_ON_ERROR").as_deref(), Ok("1" | "true"));
    let interval: u64 = std::env::var("WATCH_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(30);

    let mut known: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    for path in &paths {
        let result = read(path);
        match &result {
            Ok(text) => kit::info(&format!("loaded {path}: {}", show(text))),
            Err(reason) => {
                kit::error(&format!("cannot read {path}: {reason}"));
                if exit_on_error {
                    kit::fatal(&format!("cannot start without {path}, exiting"));
                }
            }
        }
        known.insert(path.clone(), result);
    }

    if !config.reload {
        kit::serve_forever();
    }
    loop {
        kit::sleep(interval);
        for path in &paths {
            let now = read(path);
            if known.get(path) == Some(&now) {
                continue;
            }
            match &now {
                Ok(text) => kit::info(&format!("reloaded {path}: {}", show(text))),
                Err(reason) => kit::error(&format!("cannot read {path}: {reason}")),
            }
            known.insert(path.clone(), now);
        }
    }
}
