//! What every template app needs: its image config, log lines in one format, and the two ways a
//! container process ends (exit, or serve until killed).
//!
//! Apps are ordinary programs. Time is simulated by the engine: `sleep` returns at once in real
//! time but moves the container's clock, so log lines land at the right simulated second.

use std::time::Duration;

use serde::de::DeserializeOwned;

/// The image's `config` from the scenario, passed as the first argument (JSON). An app that
/// cannot read its config exits with code 2, like a CLI given bad flags.
pub fn config<T: DeserializeOwned>() -> T {
    let raw = std::env::args().nth(1).unwrap_or_else(|| "{}".into());
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        log("FATAL", &format!("invalid image config: {e}"));
        std::process::exit(2)
    })
}

/// One log line: `INFO  message`, `ERROR message`, ... (level padded to five characters).
pub fn log(level: &str, message: &str) {
    println!("{level:<5} {message}");
}

pub fn info(message: &str) {
    log("INFO", message);
}

pub fn error(message: &str) {
    log("ERROR", message);
}

/// Log a fatal error and exit with code 1.
pub fn fatal(message: &str) -> ! {
    log("FATAL", message);
    std::process::exit(1)
}

pub fn sleep(seconds: u64) {
    std::thread::sleep(Duration::from_secs(seconds));
}

/// Keep the process alive, as a server does. The engine sees the endless sleep and records the
/// container as running for good.
pub fn serve_forever() -> ! {
    loop {
        sleep(3600);
    }
}
