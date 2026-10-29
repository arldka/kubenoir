//! `kubectl exec`: the only program inside a container is `curl`, which asks the connectivity
//! oracle where a request would end up and the listening app what it answers.

use serde_json::Value;

use crate::controllers::{self, Env};
use crate::net::{self, Outcome};
use crate::parser::ExecArgs;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

pub fn exec(args: &ExecArgs, env: &Env<'_>) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let Some(pod) = env.store.get("Pod", namespace, &args.pod)? else {
        return Ok(format!("Error from server (NotFound): pods \"{}\" not found", args.pod));
    };
    let containers: Vec<&str> = pod["spec"]["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["name"].as_str())
        .collect();
    let mut out = Vec::new();
    let container = match args.container.as_deref() {
        Some(wanted) if containers.contains(&wanted) => wanted,
        Some(wanted) => {
            return Ok(format!(
                "Error from server (BadRequest): container {wanted} is not valid for pod {}",
                args.pod
            ));
        }
        None => {
            let first = containers.first().copied().unwrap_or_default();
            if containers.len() > 1 {
                out.push(format!(
                    "Defaulted container \"{first}\" out of: {}",
                    containers.join(", ")
                ));
            }
            first
        }
    };
    if !controllers::is_running(env.store, namespace, &args.pod, container)? {
        out.push(format!(
            "error: unable to upgrade connection: container not found (\"{container}\")"
        ));
        return Ok(out.join("\n"));
    }

    let command = args.command[0].as_str();
    if command != "curl" {
        out.push(format!(
            "error: running {command} in a container is not available in this terminal (only curl is)"
        ));
        return Ok(out.join("\n"));
    }
    out.extend(curl(&args.command[1..], &pod, env)?);
    Ok(out.join("\n"))
}

/// What the player asked curl for.
#[derive(Default)]
struct Curl {
    silent: bool,
    show_errors: bool,
    include: bool,
    fail: bool,
    head: bool,
    method: Option<String>,
    url: Option<String>,
}

/// A curl failure: the message, and the exit code `kubectl exec` reports.
struct Failure(i32, String);

fn curl(args: &[String], source: &Value, env: &Env<'_>) -> Result<Vec<String>, StoreError> {
    let mut options = Curl::default();
    // `-sS` is `-s -S`.
    let args: Vec<String> = args
        .iter()
        .flat_map(|a| match a.strip_prefix('-') {
            Some(letters) if letters.len() > 1 && letters.chars().all(|c| "sSiIfLkv".contains(c)) => {
                letters.chars().map(|c| format!("-{c}")).collect()
            }
            _ => vec![a.clone()],
        })
        .collect();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-s" | "--silent" => options.silent = true,
            "-S" | "--show-error" => options.show_errors = true,
            "-i" | "--include" => options.include = true,
            "-I" | "--head" => (options.head, options.include) = (true, true),
            "-f" | "--fail" => options.fail = true,
            "-L" | "--location" | "-k" | "--insecure" | "-v" | "--verbose" => {}
            "-X" | "--request" => options.method = iter.next().cloned(),
            "-m" | "--max-time" | "--connect-timeout" => {
                iter.next();
            }
            flag if flag.starts_with('-') => {
                return Ok(vec![
                    format!("curl: option {flag}: is unknown"),
                    "curl: try 'curl --help' or 'curl --manual' for more information".into(),
                    "command terminated with exit code 2".into(),
                ]);
            }
            url => options.url = Some(url.to_string()),
        }
    }
    let Some(url) = options.url.clone() else {
        return Ok(vec![
            "curl: no URL specified".into(),
            "curl: try 'curl --help' or 'curl --manual' for more information".into(),
            "command terminated with exit code 2".into(),
        ]);
    };

    match request(&options, &url, source, env)? {
        Ok(text) => Ok(vec![text]),
        Err(Failure(code, message)) => {
            let mut out = Vec::new();
            if !options.silent || options.show_errors {
                out.push(format!("curl: ({code}) {message}"));
            }
            out.push(format!("command terminated with exit code {code}"));
            Ok(out)
        }
    }
}

fn request(options: &Curl, url: &str, source: &Value, env: &Env<'_>) -> Result<Result<String, Failure>, StoreError> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let (authority, path) = match rest.find(['/', '?']) {
        Some(at) if rest.as_bytes()[at] == b'/' => (&rest[..at], &rest[at..]),
        Some(at) => (&rest[..at], &format!("/{}", &rest[at..])[..]),
        None => (rest, "/"),
    };
    let path = path.to_string();
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host, i64::from(port)),
            Err(_) => {
                return Ok(Err(Failure(
                    3,
                    "URL rejected: Port number was not a decimal number between 0 and 65535".into(),
                )))
            }
        },
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Ok(Err(Failure(3, "URL rejected: No host part in the URL".into())));
    }

    let namespace = source["metadata"]["namespace"].as_str().unwrap_or(DEFAULT_NAMESPACE);
    let listens = |pod: &Value, port: i64| controllers::listens(env.store, pod, port);
    let outcome = net::connect(env.store, namespace, host, port, &listens)?;
    let (target_namespace, target, target_port) = match outcome {
        Outcome::NxDomain => return Ok(Err(Failure(6, format!("Could not resolve host: {host}")))),
        Outcome::Refused => {
            return Ok(Err(Failure(
                7,
                format!("Failed to connect to {host} port {port} after 2 ms: Connection refused"),
            )))
        }
        // Nothing answers, and nothing outside the cluster is simulated.
        Outcome::Timeout | Outcome::External { .. } => {
            return Ok(Err(Failure(
                28,
                format!("Failed to connect to {host} port {port} after 130000 ms: Couldn't connect to server"),
            )))
        }
        Outcome::Connected { namespace, pod, port } => (namespace, pod, port),
    };
    let Some(pod) = env.store.get("Pod", &target_namespace, &target)? else {
        return Ok(Err(Failure(
            7,
            format!("Failed to connect to {host} port {port} after 2 ms: Connection refused"),
        )));
    };
    let method = if options.head {
        "HEAD"
    } else {
        options.method.as_deref().unwrap_or("GET")
    };
    let Some(response) = controllers::answer(env, &pod, target_port, method, &path)? else {
        return Ok(Err(Failure(
            7,
            format!("Failed to connect to {host} port {port} after 2 ms: Connection refused"),
        )));
    };
    if options.fail && response.status >= 400 {
        return Ok(Err(Failure(
            22,
            format!("The requested URL returned error: {}", response.status),
        )));
    }
    let mut text = String::new();
    if options.include {
        text.push_str(&format!(
            "HTTP/1.1 {} {}\nContent-Type: text/plain\nContent-Length: {}\n\n",
            response.status,
            reason(response.status),
            response.body.len()
        ));
    }
    if !options.head {
        text.push_str(&response.body);
    }
    Ok(Ok(text.trim_end_matches('\n').to_string()))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "",
    }
}
