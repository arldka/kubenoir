//! A service that reads its PostgreSQL connection URL from the environment, connects, then
//! serves. It fails the way a real client would: not set, not a URL, no such host, nobody
//! listening there, or the server says no.
//!
//! The server is whatever the cluster has at that address: the connection goes through the
//! engine's connectivity oracle to the program that listens there (a `postgres` pod). An image
//! config with a `database` block predates that and describes the one server that exists, so the
//! host and port are compared with it instead (scenarios move off it as they are reworked).

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    name: String,
    version: String,
    #[serde(default = "default_url_env")]
    database_url_env: String,
    #[serde(default = "default_port_env")]
    port_env: String,
    #[serde(default)]
    database: Option<Database>,
}

/// The one database server that exists for this service.
#[derive(Deserialize)]
struct Database {
    host: String,
    port: u16,
    user: String,
    name: String,
    #[serde(default)]
    password: Option<String>,
}

fn default_url_env() -> String {
    "DATABASE_URL".into()
}

fn default_port_env() -> String {
    "PORT".into()
}

/// The parts of `postgres://user[:password]@host[:port][/database]`. Missing parts stay `None`
/// and never match the server: this client has no defaults to fall back on.
#[derive(Debug, PartialEq)]
struct Url<'a> {
    user: Option<&'a str>,
    password: Option<&'a str>,
    host: &'a str,
    port: Option<u16>,
    database: Option<&'a str>,
}

fn parse(url: &str) -> Result<Url<'_>, &'static str> {
    let rest = url
        .strip_prefix("postgres://")
        .ok_or("the scheme must be postgres://")?;
    let (authority, database) = match rest.split_once('/') {
        Some((a, d)) => (a, Some(d)),
        None => (rest, None),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((user, pass)) => (Some(user), Some(pass)),
            None => (Some(u), None),
        },
        None => (None, None),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse().map_err(|_| "the port must be a number")?)),
        None => (hostport, None),
    };
    if host.is_empty() {
        return Err("the host is missing");
    }
    Ok(Url {
        user,
        password,
        host,
        port,
        database,
    })
}

/// Percent-encode what would break `key=value&key=value`.
fn encode(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '%' | '&' | '=' | ' ' => format!("%{:02X}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// Ask the server at `host:port` to let `user` in. Only an accepted login returns.
fn log_in(parsed: &Url<'_>, url: &str) {
    let path = format!(
        "user={}&database={}&password={}",
        encode(parsed.user.unwrap_or_default()),
        encode(parsed.database.unwrap_or_default()),
        encode(parsed.password.unwrap_or_default())
    );
    let port = parsed.port.unwrap_or(5432);
    match kit::ask(parsed.host, port, "STARTUP", &path) {
        Ok(reply) if reply.status == 200 => {}
        Ok(reply) => {
            kit::error(&format!("connection to the database failed: {}", reply.body));
            kit::fatal("cannot start without a database connection, exiting");
        }
        Err(kit::NetError::NxDomain) => {
            kit::error(&format!(
                "could not connect to {url}: could not translate host name \"{}\" to address: Name or service not known",
                parsed.host
            ));
            kit::fatal("cannot start without a database connection, exiting");
        }
        Err(kit::NetError::Refused) => {
            kit::error(&format!(
                "could not connect to {url}: connection to server at \"{}\", port {port} failed: Connection refused",
                parsed.host
            ));
            kit::fatal("cannot start without a database connection, exiting");
        }
        Err(kit::NetError::Timeout) => {
            kit::sleep(5);
            kit::error(&format!(
                "could not connect to {url}: connection to server at \"{}\", port {port} failed: timeout expired",
                parsed.host
            ));
            kit::fatal("cannot start without a database connection, exiting");
        }
    }
}

fn main() {
    let config: Config = kit::config();
    let listen_port = std::env::var(&config.port_env)
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok());
    if kit::answering_request() {
        // Asked a question, not started: answer without connecting to anything.
        kit::serve(listen_port.unwrap_or(0), |request: &kit::Request| {
            match request.path.as_str() {
                "/healthz" => kit::Response::new(200, "ok"),
                _ => kit::Response::new(200, &format!("{} {}", config.name, config.version)),
            }
        });
    }
    kit::info(&format!("{} {} starting (pid 1)", config.name, config.version));
    kit::info("loading configuration from environment");
    let url = std::env::var(&config.database_url_env).unwrap_or_default();
    kit::sleep(1);

    if url.is_empty() {
        kit::error(&format!(
            "required environment variable {} is not set",
            config.database_url_env
        ));
        kit::fatal("cannot start without a database connection, exiting");
    }
    let parsed = match parse(&url) {
        Ok(parsed) => parsed,
        Err(reason) => {
            kit::error(&format!("invalid {} {url:?}: {reason}", config.database_url_env));
            kit::fatal("cannot start without a database connection, exiting");
        }
    };

    kit::info(&format!("connecting to {url}"));
    match &config.database {
        Some(db) => check_static(db, &parsed, &url),
        None => log_in(&parsed, &url),
    }

    kit::info(&format!("connected to {url}"));
    let port = std::env::var(&config.port_env).unwrap_or_default();
    kit::info(&format!("listening on :{port}"));
    kit::serve(listen_port.unwrap_or(0), |request: &kit::Request| {
        match request.path.as_str() {
            "/healthz" => kit::Response::new(200, "ok"),
            _ => kit::Response::new(200, &format!("{} {}", config.name, config.version)),
        }
    })
}

/// The earlier model: one server exists, described by the image config.
fn check_static(db: &Database, parsed: &Url<'_>, url: &str) {
    if parsed.host != db.host || parsed.port != Some(db.port) {
        kit::error(&format!(
            "could not connect to {url}: connection refused (check the host and port)"
        ));
        kit::fatal("cannot start without a database connection, exiting");
    }
    if parsed.user != Some(db.user.as_str())
        || parsed.database != Some(db.name.as_str())
        || parsed.password != db.password.as_deref()
    {
        kit::error("the database server rejected the connection: unknown role or database");
        kit::fatal("cannot start without a database connection, exiting");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_part() {
        assert_eq!(
            parse("postgres://u:p@h:5432/db"),
            Ok(Url {
                user: Some("u"),
                password: Some("p"),
                host: "h",
                port: Some(5432),
                database: Some("db"),
            })
        );
    }

    #[test]
    fn missing_parts_are_none() {
        let url = parse("postgres://x").unwrap();
        assert_eq!(
            (url.user, url.password, url.host, url.port, url.database),
            (None, None, "x", None, None)
        );
        assert_eq!(parse("postgres://u:@h").unwrap().password, Some(""));
        assert_eq!(parse("postgres://h:1/").unwrap().database, Some(""));
    }

    #[test]
    fn rejects_what_is_not_a_postgres_url() {
        assert!(parse("x").is_err());
        assert!(parse("POSTGRES://h").is_err());
        assert!(parse("postgres://h:port").is_err());
        assert!(parse("postgres://u@").is_err());
    }
}
