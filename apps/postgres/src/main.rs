//! A PostgreSQL server in the manner of the official image: the superuser comes from
//! `POSTGRES_USER` (default `postgres`), its password from `POSTGRES_PASSWORD` (required) and the
//! database from `POSTGRES_DB` (default: the user). It listens on `port` (config, default 5432)
//! and lets a client in only when role, database and password all match.

use kit::{Request, Response};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_port")]
    port: u16,
}

fn default_port() -> u16 {
    5432
}

fn setting(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn decode(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            out.push(u8::from_str_radix(&hex, 16).map_or('%', char::from));
        } else {
            out.push(c);
        }
    }
    out
}

/// The `user`, `database` and `password` of a startup message (`key=value&...`).
fn startup(path: &str) -> (String, String, String) {
    let field = |name: &str| {
        path.split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| decode(v))
            .unwrap_or_default()
    };
    (field("user"), field("database"), field("password"))
}

fn main() {
    let config: Config = kit::config();
    let user = setting("POSTGRES_USER").unwrap_or_else(|| "postgres".into());
    let database = setting("POSTGRES_DB").unwrap_or_else(|| user.clone());
    let password = setting("POSTGRES_PASSWORD");

    if !kit::answering_request() {
        let Some(_) = password else {
            kit::log(
                "Error:",
                "Database is uninitialized and superuser password is not specified.",
            );
            kit::log(
                "",
                "       You must specify POSTGRES_PASSWORD to a non-empty value for the superuser.",
            );
            std::process::exit(1);
        };
        kit::info("PostgreSQL init process complete; ready for start up.");
        kit::sleep(1);
        kit::info("database system is ready to accept connections");
    }

    kit::serve(config.port, |request: &Request| {
        let (who, db, secret) = startup(&request.path);
        if who != user {
            Response::new(401, &format!("FATAL:  role \"{who}\" does not exist"))
        } else if db != database {
            Response::new(401, &format!("FATAL:  database \"{db}\" does not exist"))
        } else if password.as_deref() != Some(secret.as_str()) {
            Response::new(
                401,
                &format!("FATAL:  password authentication failed for user \"{who}\""),
            )
        } else {
            Response::new(200, "ok")
        }
    })
}
