//! A small web service. It listens on the port named by the environment variable `portEnv`
//! (default `defaultPort`), answers `/` with its banner, `/healthz` with `ok`, and anything else
//! with 404. A bad port value in the environment makes it exit, like a real server would.

use kit::{Request, Response};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    name: String,
    version: String,
    #[serde(default)]
    port_env: Option<String>,
    #[serde(default = "default_port")]
    default_port: u16,
}

fn default_port() -> u16 {
    8080
}

fn main() {
    let config: Config = kit::config();
    let port = match config.port_env.as_deref().and_then(|name| std::env::var(name).ok()) {
        None => config.default_port,
        Some(text) => text.trim().parse().unwrap_or_else(|_| {
            kit::fatal(&format!("invalid port {text:?}"));
        }),
    };
    if !kit::answering_request() {
        kit::info(&format!("{} {} starting (pid 1)", config.name, config.version));
        kit::sleep(1);
        // A slow starter: `WARMUP_SECS` of loading before it accepts connections.
        let warmup = std::env::var("WARMUP_SECS")
            .ok()
            .and_then(|w| w.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if warmup > 0 {
            kit::info(&format!("warming up ({warmup}s)"));
            kit::sleep(warmup);
        }
        kit::info(&format!("listening on :{port}"));
    }
    kit::serve(port, |request: &Request| match request.path.as_str() {
        // `exec` probes: the one command this server understands.
        "true" if request.method == "EXEC" => Response::new(200, ""),
        _ if request.method == "EXEC" => Response::new(500, "executable file not found in $PATH"),
        "/" => Response::new(200, &format!("{} {}", config.name, config.version)),
        "/healthz" => Response::new(200, "ok"),
        _ => Response::new(404, "not found"),
    })
}
