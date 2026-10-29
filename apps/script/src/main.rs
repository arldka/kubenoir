//! A container whose output does not depend on anything: fixed lines at fixed seconds after start,
//! then an exit code or a process that keeps running. For batch jobs, sidecars and filler.

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    logs: Vec<Line>,
    exit: Option<Exit>,
}

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    at: u64,
    line: String,
}

#[derive(Deserialize)]
struct Exit {
    #[serde(default)]
    at: u64,
    code: i32,
}

fn main() {
    let mut config: Config = kit::config();
    config.logs.sort_by_key(|l| l.at); // stable: equal offsets keep authored order
    let mut now = 0;
    let mut wait_until = |at: u64| {
        if at > now {
            kit::sleep(at - now);
            now = at;
        }
    };
    for line in &config.logs {
        wait_until(line.at);
        println!("{}", line.line);
    }
    match config.exit {
        Some(exit) => {
            wait_until(exit.at);
            std::process::exit(exit.code)
        }
        None => kit::serve_forever(),
    }
}
