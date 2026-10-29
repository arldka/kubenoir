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

/// Why a file operation failed, worded as the C library would (the WASI error numbers, not the
/// host's, so the text is the same wherever the app runs).
pub fn io_reason(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(2) => "Permission denied".into(),
        Some(20) => "File exists".into(),
        Some(31) => "Is a directory".into(),
        Some(44) => "No such file or directory".into(),
        Some(54) => "Not a directory".into(),
        Some(55) => "Directory not empty".into(),
        Some(69) => "Read-only file system".into(),
        _ => error.to_string(),
    }
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

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "detective")]
extern "C" {
    #[link_name = "listen"]
    fn host_listen(port: i32);
}

/// Declare the port this process accepts connections on. Nothing is bound: the engine's
/// connectivity oracle uses the declaration to tell a refused connection from an accepted one.
pub fn listen(port: u16) {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: the host function only records the number.
    unsafe {
        host_listen(i32::from(port));
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = port;
}

/// A request to a server app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
}

/// What a server app answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn new(status: u16, body: &str) -> Self {
        Response {
            status,
            body: body.to_string(),
        }
    }
}

/// Run as a server on `port`. Started normally, the process declares the port and serves until
/// killed. Asked to answer a request (the engine runs the same program again with
/// `request <method> <path>` after the config), it prints `status: <code>` and the body from
/// `handler`, and exits.
pub fn serve(port: u16, handler: impl Fn(&Request) -> Response) -> ! {
    let args: Vec<String> = std::env::args().collect();
    if args.get(2).map(String::as_str) == Some("request") {
        let request = Request {
            method: args.get(3).cloned().unwrap_or_else(|| "GET".into()),
            path: args.get(4).cloned().unwrap_or_else(|| "/".into()),
        };
        let response = handler(&request);
        println!("status: {}", response.status);
        if !response.body.is_empty() {
            println!("{}", response.body);
        }
        std::process::exit(0)
    }
    listen(port);
    serve_forever()
}

/// Whether the engine started this run to answer a request rather than to be a container.
pub fn answering_request() -> bool {
    std::env::args().nth(2).as_deref() == Some("request")
}
