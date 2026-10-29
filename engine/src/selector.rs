//! Label selectors (`-l app=web,tier!=db,env in (a,b),!debug`), as kubectl accepts them.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Requirement {
    Equals(String, String),
    NotEquals(String, String),
    In(String, Vec<String>),
    NotIn(String, Vec<String>),
    Exists(String),
    Absent(String),
}

/// All requirements must hold. The empty selector matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selector(Vec<Requirement>);

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn valid_value(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Split on commas that are not inside parentheses.
fn terms(text: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0usize, 0, Vec::new());
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

fn values(list: &str) -> Option<Vec<String>> {
    let inner = list.trim().strip_prefix('(')?.strip_suffix(')')?;
    let values: Vec<String> = inner.split(',').map(|v| v.trim().to_string()).collect();
    values.iter().all(|v| valid_value(v)).then_some(values)
}

impl Selector {
    pub fn parse(text: &str) -> Result<Selector, String> {
        let bad = || format!("couldn't parse the selector string \"{text}\": invalid selector");
        let mut requirements = Vec::new();
        if text.trim().is_empty() {
            return Ok(Selector(requirements));
        }
        for term in terms(text) {
            let term = term.trim();
            let requirement = if let Some((key, rest)) = term.split_once(" notin ") {
                Requirement::NotIn(key.trim().to_string(), values(rest).ok_or_else(bad)?)
            } else if let Some((key, rest)) = term.split_once(" in ") {
                Requirement::In(key.trim().to_string(), values(rest).ok_or_else(bad)?)
            } else if let Some((key, value)) = term.split_once("!=") {
                Requirement::NotEquals(key.trim().to_string(), value.trim().to_string())
            } else if let Some((key, value)) = term.split_once("==").or_else(|| term.split_once('=')) {
                Requirement::Equals(key.trim().to_string(), value.trim().to_string())
            } else if let Some(key) = term.strip_prefix('!') {
                Requirement::Absent(key.trim().to_string())
            } else {
                Requirement::Exists(term.to_string())
            };
            let (key, ok) = match &requirement {
                Requirement::Equals(k, v) | Requirement::NotEquals(k, v) => (k, valid_value(v)),
                Requirement::In(k, _) | Requirement::NotIn(k, _) | Requirement::Exists(k) | Requirement::Absent(k) => {
                    (k, true)
                }
            };
            if !ok || !valid_key(key) {
                return Err(bad());
            }
            requirements.push(requirement);
        }
        Ok(Selector(requirements))
    }

    /// Whether a manifest's `metadata.labels` satisfy the selector.
    pub fn matches(&self, manifest: &Value) -> bool {
        let labels = &manifest["metadata"]["labels"];
        let label = |key: &String| labels.get(key).and_then(Value::as_str);
        self.0.iter().all(|r| match r {
            Requirement::Equals(k, v) => label(k) == Some(v.as_str()),
            Requirement::NotEquals(k, v) => label(k) != Some(v.as_str()),
            Requirement::In(k, vs) => label(k).is_some_and(|l| vs.iter().any(|v| v == l)),
            Requirement::NotIn(k, vs) => label(k).is_none_or(|l| !vs.iter().any(|v| v == l)),
            Requirement::Exists(k) => label(k).is_some(),
            Requirement::Absent(k) => label(k).is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(labels: Value) -> Value {
        json!({"metadata": {"labels": labels}})
    }

    fn selects(selector: &str, labels: Value) -> bool {
        Selector::parse(selector).unwrap().matches(&obj(labels))
    }

    #[test]
    fn equality_and_inequality() {
        assert!(selects("app=web", json!({"app": "web"})));
        assert!(selects("app==web", json!({"app": "web", "x": "y"})));
        assert!(!selects("app=web", json!({"app": "db"})));
        assert!(!selects("app=web", json!({})));
        assert!(selects("app!=web", json!({"app": "db"})));
        assert!(selects("app!=web", json!({})), "a missing label is not equal");
        assert!(selects("app=web,tier=front", json!({"app": "web", "tier": "front"})));
        assert!(!selects("app=web,tier=front", json!({"app": "web"})));
    }

    #[test]
    fn sets_and_existence() {
        assert!(selects("env in (a, b)", json!({"env": "b"})));
        assert!(!selects("env in (a,b)", json!({"env": "c"})));
        assert!(!selects("env in (a,b)", json!({})));
        assert!(selects("env notin (a,b)", json!({"env": "c"})));
        assert!(selects("env notin (a,b)", json!({})));
        assert!(selects("debug", json!({"debug": ""})));
        assert!(!selects("debug", json!({})));
        assert!(selects("!debug", json!({})));
        assert!(!selects("!debug", json!({"debug": "1"})));
        assert!(selects(
            "env in (a,b),app=web,!debug",
            json!({"env": "a", "app": "web"})
        ));
    }

    #[test]
    fn empty_matches_everything_and_unlabelled_objects_work() {
        assert_eq!(Selector::parse("").unwrap(), Selector::default());
        assert!(Selector::default().matches(&json!({})));
        assert!(!Selector::parse("a=b").unwrap().matches(&json!({"metadata": {}})));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["a=b c", "=x", "a in b", "a in (b", "!", "a=b,,c", "a b=c"] {
            assert_eq!(
                Selector::parse(bad),
                Err(format!(
                    "couldn't parse the selector string \"{bad}\": invalid selector"
                )),
                "{bad}"
            );
        }
    }
}
