//! The environment a container starts with: `env` and `envFrom`, with `valueFrom` references to
//! ConfigMaps, Secrets and the pod's own fields resolved against the cluster *at the moment the
//! container starts*, as the kubelet does. What a running container already has is never
//! touched; a restart reads the current values.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Container, EnvVarSource, Pod};
use serde_json::Value;

use crate::shell::base64_decode;
use crate::store::{ClusterStore, StoreError};

/// What the container gets, and what the kubelet should warn about.
pub struct Resolved {
    pub env: BTreeMap<String, String>,
    /// `Warning` events to record (keys skipped because they cannot be variable names).
    pub warnings: Vec<String>,
}

/// `Ok(Err(message))` is a configuration error: the container cannot be created yet.
pub type Outcome = Result<Result<Resolved, String>, StoreError>;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The text values of a ConfigMap, or of a Secret decoded from base64 (byte for byte; a
/// trailing newline is part of the value).
fn values(object: &Value, secret: bool) -> BTreeMap<String, String> {
    object["data"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let text = value.as_str()?;
            let text = if secret {
                String::from_utf8_lossy(&base64_decode(text)?).into_owned()
            } else {
                text.to_string()
            };
            Some((key.clone(), text))
        })
        .collect()
}

struct Source<'a> {
    kind: &'static str,
    /// How the kubelet words it in errors: `configmap`, `secret`.
    noun: &'static str,
    name: &'a str,
    optional: bool,
}

/// Fetch the source's values; `None` when it is optional and absent.
fn fetch(
    store: &ClusterStore,
    namespace: &str,
    source: &Source<'_>,
) -> Result<Result<Option<BTreeMap<String, String>>, String>, StoreError> {
    Ok(match store.get(source.kind, namespace, source.name)? {
        Some(object) => Ok(Some(values(&object, source.kind == "Secret"))),
        None if source.optional => Ok(None),
        None => Err(format!("{} \"{}\" not found", source.noun, source.name)),
    })
}

/// `$(NAME)` in a value is replaced by an earlier variable of that name; `$$` is a literal `$`.
fn expand(value: &str, env: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('$') => {
                out.push('$');
                i += 2;
            }
            Some('(') => match chars[i + 2..].iter().position(|c| *c == ')') {
                Some(len) => {
                    let name: String = chars[i + 2..i + 2 + len].iter().collect();
                    match env.get(&name) {
                        Some(found) => out.push_str(found),
                        None => out.push_str(&format!("$({name})")),
                    }
                    i += len + 3;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// A field of the pod, by the paths the downward API allows.
fn pod_field(pod: &Pod, path: &str) -> Result<String, String> {
    let meta = &pod.metadata;
    let status = pod.status.as_ref();
    let spec = pod.spec.as_ref();
    let map_entry =
        |map: Option<&BTreeMap<String, String>>, key: &str| map.and_then(|m| m.get(key)).cloned().unwrap_or_default();
    let all = |map: Option<&BTreeMap<String, String>>| {
        map.into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}=\"{v}\""))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(match path {
        "metadata.name" => meta.name.clone().unwrap_or_default(),
        "metadata.namespace" => meta.namespace.clone().unwrap_or_default(),
        "metadata.uid" => meta.uid.clone().unwrap_or_default(),
        "metadata.labels" => all(meta.labels.as_ref()),
        "metadata.annotations" => all(meta.annotations.as_ref()),
        "spec.nodeName" => spec.and_then(|s| s.node_name.clone()).unwrap_or_default(),
        "spec.serviceAccountName" => spec
            .and_then(|s| s.service_account_name.clone())
            .unwrap_or_else(|| "default".into()),
        "status.hostIP" => status.and_then(|s| s.host_ip.clone()).unwrap_or_default(),
        "status.podIP" | "status.podIPs" => status.and_then(|s| s.pod_ip.clone()).unwrap_or_default(),
        other => {
            let keyed = |prefix: &str| other.strip_prefix(prefix).and_then(|r| r.strip_suffix("']"));
            if let Some(key) = keyed("metadata.labels['") {
                map_entry(meta.labels.as_ref(), key)
            } else if let Some(key) = keyed("metadata.annotations['") {
                map_entry(meta.annotations.as_ref(), key)
            } else {
                return Err(format!("unsupported fieldPath: {other}"));
            }
        }
    })
}

fn from_source(
    store: &ClusterStore,
    pod: &Pod,
    namespace: &str,
    from: &EnvVarSource,
) -> Result<Result<Option<String>, String>, StoreError> {
    if let Some(field) = &from.field_ref {
        return Ok(pod_field(pod, &field.field_path).map(Some));
    }
    if from.resource_field_ref.is_some() {
        return Ok(Err("resourceFieldRef is not available in this terminal".into()));
    }
    let (kind, noun, name, key, optional) = if let Some(r) = &from.config_map_key_ref {
        (
            "ConfigMap",
            "configmap",
            r.name.clone(),
            r.key.clone(),
            r.optional.unwrap_or(false),
        )
    } else if let Some(r) = &from.secret_key_ref {
        (
            "Secret",
            "secret",
            r.name.clone(),
            r.key.clone(),
            r.optional.unwrap_or(false),
        )
    } else {
        return Ok(Ok(None));
    };
    let source = Source {
        kind,
        noun,
        name: &name,
        optional,
    };
    Ok(match fetch(store, namespace, &source)? {
        Err(message) => Err(message),
        Ok(None) => Ok(None),
        Ok(Some(data)) => match data.get(&key) {
            Some(value) => Ok(Some(value.clone())),
            None if optional => Ok(None),
            None => Err(format!("couldn't find key {key} in {kind} {namespace}/{name}")),
        },
    })
}

pub fn resolve(store: &ClusterStore, pod: &Pod, container: &Container) -> Outcome {
    let namespace = pod.metadata.namespace.clone().unwrap_or_default();
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings = Vec::new();

    for source in container.env_from.iter().flatten() {
        let prefix = source.prefix.clone().unwrap_or_default();
        let (kind, noun, reference) = match (&source.config_map_ref, &source.secret_ref) {
            (Some(r), _) => ("ConfigMap", "configMap", (r.name.clone(), r.optional.unwrap_or(false))),
            (None, Some(r)) => ("Secret", "secret", (r.name.clone(), r.optional.unwrap_or(false))),
            (None, None) => continue,
        };
        let (name, optional) = reference;
        let lookup = Source {
            kind,
            noun: if kind == "Secret" { "secret" } else { "configmap" },
            name: &name,
            optional,
        };
        let Some(data) = (match fetch(store, &namespace, &lookup)? {
            Err(message) => return Ok(Err(message)),
            Ok(data) => data,
        }) else {
            continue;
        };
        let mut skipped = Vec::new();
        for (key, value) in data {
            if valid_name(&key) {
                env.insert(format!("{prefix}{key}"), value);
            } else {
                skipped.push(key);
            }
        }
        if !skipped.is_empty() {
            warnings.push(format!(
                "Keys [{}] from the EnvFrom {noun} {namespace}/{name} were skipped since they are considered invalid environment variable names.",
                skipped.join(", ")
            ));
        }
    }

    for var in container.env.iter().flatten() {
        match (&var.value, &var.value_from) {
            (Some(value), _) => {
                env.insert(var.name.clone(), expand(value, &env));
            }
            (None, None) => {
                env.insert(var.name.clone(), String::new());
            }
            (None, Some(from)) => match from_source(store, pod, &namespace, from)? {
                Err(message) => return Ok(Err(message)),
                Ok(Some(value)) => {
                    env.insert(var.name.clone(), value);
                }
                Ok(None) => {}
            },
        }
    }
    Ok(Ok(Resolved { env, warnings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn dollar_parenthesis_expands_earlier_variables() {
        let env = vars(&[("HOST", "db"), ("PORT", "5432")]);
        assert_eq!(expand("postgres://$(HOST):$(PORT)/x", &env), "postgres://db:5432/x");
        assert_eq!(expand("$(NOPE) stays", &env), "$(NOPE) stays");
        assert_eq!(expand("$$(HOST) is literal", &env), "$(HOST) is literal");
        assert_eq!(expand("cost: 5$ and $(HOST", &env), "cost: 5$ and $(HOST");
        assert_eq!(expand("$(HOST)$(HOST)", &env), "dbdb");
    }

    #[test]
    fn variable_names() {
        for good in ["A", "a_b", "A-B", "creds.txt", "_x"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["", "1abc", "has space", "a/b", "é"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn secrets_are_decoded_exactly() {
        let secret = serde_json::json!({"data": {"with": "cGFzcwo=", "without": "cGFzcw==", "bad": "***"}});
        let decoded = values(&secret, true);
        assert_eq!(decoded["with"], "pass\n");
        assert_eq!(decoded["without"], "pass");
        assert!(!decoded.contains_key("bad"));
        assert_eq!(
            values(&serde_json::json!({"data": {"a": "plain"}}), false)["a"],
            "plain"
        );
    }
}
