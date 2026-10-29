//! Can an image be pulled? The scenario's image catalog says which images exist and which need
//! credentials; a pod brings credentials as docker-registry Secrets in `imagePullSecrets`.

use k8s_openapi::api::core::v1::Pod;
use serde_json::Value;

use super::Env;
use crate::shell::base64_decode;
use crate::store::StoreError;

/// The registry host of an image reference: its first path segment if that looks like a host,
/// else Docker Hub.
fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => first,
        _ => "docker.io",
    }
}

/// The reference containerd would resolve: host and repository path spelled out.
fn full_reference(image: &str) -> String {
    match image.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => image.to_string(),
        Some(_) => format!("docker.io/{image}"),
        None => format!("docker.io/library/{image}"),
    }
}

/// The host in a docker config key (`https://index.docker.io/v1/` → `index.docker.io`).
fn config_key_host(key: &str) -> &str {
    let rest = key.split_once("://").map_or(key, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

/// Whether one pull secret holds the credentials for the image's registry.
fn secret_grants(secret: &Value, host: &str, username: &str, password: &str) -> bool {
    if secret["type"] != "kubernetes.io/dockerconfigjson" {
        return false;
    }
    let Some(config) = secret["data"][".dockerconfigjson"]
        .as_str()
        .and_then(base64_decode)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    config["auths"].as_object().into_iter().flatten().any(|(key, entry)| {
        let same_host =
            config_key_host(key) == host || (host == "docker.io" && config_key_host(key).ends_with("docker.io"));
        let from_auth = entry["auth"]
            .as_str()
            .and_then(base64_decode)
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|pair| pair.split_once(':').map(|(u, p)| (u.to_string(), p.to_string())));
        let given = (entry["username"].as_str(), entry["password"].as_str());
        let matches = from_auth.as_ref().is_some_and(|(u, p)| u == username && p == password)
            || given == (Some(username), Some(password));
        same_host && matches
    })
}

/// `Ok(Err(message))`: the pull fails, with the text the kubelet records.
pub fn check_pull(env: &Env<'_>, pod: &Pod, image: &str) -> Result<Result<(), String>, StoreError> {
    let spec = env.config.images.get(image);
    let full = full_reference(image);
    let not_found = || {
        format!(
            "Failed to pull image \"{image}\": rpc error: code = NotFound desc = failed to pull and unpack image \"{full}\": failed to resolve reference \"{full}\": {full}: not found"
        )
    };
    match spec {
        None if env.config.closed_catalog => return Ok(Err(not_found())),
        Some(spec) if !spec.exists => return Ok(Err(not_found())),
        _ => {}
    }
    let Some(auth) = spec.and_then(|s| s.auth.as_ref()) else {
        return Ok(Ok(()));
    };

    let namespace = pod.metadata.namespace.clone().unwrap_or_default();
    let host = registry_host(image);
    let secrets = pod.spec.as_ref().and_then(|s| s.image_pull_secrets.as_ref());
    for reference in secrets.into_iter().flatten() {
        let Some(secret) = env.store.get("Secret", &namespace, &reference.name)? else {
            continue;
        };
        if secret_grants(&secret, host, &auth.username, &auth.password) {
            return Ok(Ok(()));
        }
    }
    Ok(Err(format!(
        "Failed to pull image \"{image}\": rpc error: code = Unknown desc = failed to pull and unpack image \"{full}\": failed to resolve reference \"{full}\": pull access denied, repository does not exist or may require authorization: server message: insufficient_scope: authorization failed"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_hosts_and_full_references() {
        assert_eq!(registry_host("nginx:1.27"), "docker.io");
        assert_eq!(registry_host("team/app:1"), "docker.io");
        assert_eq!(registry_host("registry.example.com/team/app:1"), "registry.example.com");
        assert_eq!(registry_host("localhost:5000/app"), "localhost:5000");
        assert_eq!(full_reference("nginx:1.27"), "docker.io/library/nginx:1.27");
        assert_eq!(full_reference("team/app:1"), "docker.io/team/app:1");
        assert_eq!(
            full_reference("registry.example.com/app:1"),
            "registry.example.com/app:1"
        );
        assert_eq!(config_key_host("https://index.docker.io/v1/"), "index.docker.io");
        assert_eq!(config_key_host("registry.example.com"), "registry.example.com");
    }

    #[test]
    fn a_secret_must_be_for_the_registry_with_the_right_credentials() {
        let secret = |config: Value| {
            json!({"type": "kubernetes.io/dockerconfigjson",
                   "data": {".dockerconfigjson": crate::shell::base64_encode(config.to_string().as_bytes())}})
        };
        let good = secret(json!({"auths": {"registry.example.com": {"username": "u", "password": "p"}}}));
        assert!(secret_grants(&good, "registry.example.com", "u", "p"));
        assert!(!secret_grants(&good, "registry.example.com", "u", "wrong"));
        assert!(!secret_grants(&good, "other.example.com", "u", "p"));
        let by_auth = secret(
            json!({"auths": {"https://registry.example.com/v2/": {"auth": crate::shell::base64_encode(b"u:p")}}}),
        );
        assert!(secret_grants(&by_auth, "registry.example.com", "u", "p"));
        let hub = secret(json!({"auths": {"https://index.docker.io/v1/": {"username": "u", "password": "p"}}}));
        assert!(secret_grants(&hub, "docker.io", "u", "p"));
        let mut opaque = good.clone();
        opaque["type"] = json!("Opaque");
        assert!(!secret_grants(&opaque, "registry.example.com", "u", "p"));
        assert!(!secret_grants(
            &json!({"type": "kubernetes.io/dockerconfigjson", "data": {".dockerconfigjson": "!!!"}}),
            "h",
            "u",
            "p"
        ));
    }
}
