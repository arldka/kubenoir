//! `kubectl patch`: strategic merge, JSON merge (RFC 7386) and JSON patch (RFC 6902), applied to
//! the stored object and written back through the API server's update path, so a patch is
//! validated exactly like `apply` or `replace` (pod immutability included).

use serde_json::{Map, Value};

use crate::api::{self, ApiError};
use crate::controllers::Env;
use crate::judge::Touched;
use crate::parser::{PatchArgs, PatchType};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

pub fn patch(args: &PatchArgs, env: &Env<'_>) -> Result<(String, Vec<Touched>), StoreError> {
    let Some(info) = api::kind_for_arg(&args.resource) else {
        return Ok((
            format!("error: the server doesn't have a resource type \"{}\"", args.resource),
            Vec::new(),
        ));
    };
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let stored_namespace = if info.namespaced { namespace } else { "" };
    let Some(live) = env.store.get(info.kind, stored_namespace, &args.name)? else {
        let error = ApiError::NotFound {
            resource: info.resource(),
            name: args.name.clone(),
        };
        return Ok((error.to_string(), Vec::new()));
    };
    let body: Value = match serde_yaml_ng::from_str(&args.patch) {
        Ok(body) => body,
        Err(e) => return Ok((format!("error: unable to parse \"{}\": {e}", args.patch), Vec::new())),
    };

    let mut patched = live.clone();
    let applied = match args.patch_type {
        PatchType::Merge => {
            merge_patch(&mut patched, &body);
            Ok(())
        }
        PatchType::Strategic => {
            if !body.is_object() {
                Err("error: unable to parse patch: a strategic merge patch must be an object".to_string())
            } else {
                strategic_merge(&mut patched, &body);
                Ok(())
            }
        }
        PatchType::Json => json_patch(&mut patched, &body),
    };
    if let Err(message) = applied {
        return Ok((message, Vec::new()));
    }

    match api::update(env.store, &patched, namespace) {
        Ok(updated) => {
            let suffix = if updated.changed { "" } else { " (no change)" };
            Ok((
                format!("{}/{} patched{suffix}", info.name_prefix(), updated.name),
                vec![(info.kind, updated.name)],
            ))
        }
        Err(ApiError::Store(e)) => Err(e),
        // kubectl prints validation failures as the bare message.
        Err(e @ ApiError::Invalid { .. }) => Ok((e.message(), Vec::new())),
        Err(e) => Ok((e.to_string(), Vec::new())),
    }
}

/// RFC 7386: objects merge, `null` removes, anything else replaces.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let target = target.as_object_mut().expect("just made an object");
    for (key, value) in fields {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// The key list items are merged by in a strategic merge patch, by the list's field name (and,
/// for `ports`, by what the items hold: container ports vs Service ports).
fn merge_key(field: &str, items: &[Value]) -> Option<&'static str> {
    match field {
        "containers" | "initContainers" | "ephemeralContainers" | "env" | "volumes" | "imagePullSecrets" => {
            Some("name")
        }
        "volumeMounts" | "volumeDevices" => Some("mountPath"),
        "ports" if items.iter().any(|p| p.get("containerPort").is_some()) => Some("containerPort"),
        "ports" => Some("port"),
        _ => None,
    }
}

/// Kubernetes' strategic merge patch: like a JSON merge patch, except that lists with a merge key
/// (containers by name, env by name, ...) are merged item by item, and an item with
/// `$patch: delete` is removed.
pub fn strategic_merge(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let target = target.as_object_mut().expect("just made an object");
    for (key, value) in fields {
        if key.starts_with("$setElementOrder/") || key == "$retainKeys" {
            continue;
        }
        match value {
            Value::Null => {
                target.remove(key);
            }
            Value::Array(items) => match merge_key(key, items) {
                Some(merge_key) => {
                    let existing = target.entry(key.clone()).or_insert_with(|| Value::Array(Vec::new()));
                    merge_list(existing, items, merge_key);
                }
                None => {
                    target.insert(key.clone(), value.clone());
                }
            },
            _ => strategic_merge(target.entry(key.clone()).or_insert(Value::Null), value),
        }
    }
}

fn merge_list(existing: &mut Value, items: &[Value], merge_key: &str) {
    if !existing.is_array() {
        *existing = Value::Array(Vec::new());
    }
    let list = existing.as_array_mut().expect("just made an array");
    for item in items {
        let key = &item[merge_key];
        let position = list.iter().position(|e| &e[merge_key] == key);
        if item.get("$patch").and_then(Value::as_str) == Some("delete") {
            if let Some(i) = position {
                list.remove(i);
            }
            continue;
        }
        match position {
            Some(i) => strategic_merge(&mut list[i], item),
            None => list.push(item.clone()),
        }
    }
}

/// RFC 6902. `Err` is what kubectl prints when an operation does not apply.
pub fn json_patch(target: &mut Value, ops: &Value) -> Result<(), String> {
    let Some(ops) = ops.as_array() else {
        return Err("error: unable to parse patch: a JSON patch must be a list of operations".into());
    };
    for op in ops {
        let name = op["op"].as_str().unwrap_or_default();
        let path = op["path"].as_str().unwrap_or_default();
        let fail = |why: &str| {
            Err(format!(
                "Error from server: jsonpatch {name} operation does not apply: {why}"
            ))
        };
        let result = match name {
            "add" => add(target, path, op["value"].clone()),
            "remove" => remove(target, path).map(|_| ()),
            "replace" => {
                if pointer(target, path).is_none() {
                    return fail(&format!("doc is missing key: {path}"));
                }
                remove(target, path).and_then(|_| add(target, path, op["value"].clone()))
            }
            "move" | "copy" => {
                let from = op["from"].as_str().unwrap_or_default();
                let Some(value) = pointer(target, from).cloned() else {
                    return fail(&format!("doc is missing from path: {from}"));
                };
                if name == "move" {
                    if let Err(why) = remove(target, from) {
                        return fail(&why);
                    }
                }
                add(target, path, value)
            }
            "test" => match pointer(target, path) {
                Some(value) if *value == op["value"] => Ok(()),
                _ => return Err(format!("Error from server: testing value {path} failed")),
            },
            other => return Err(format!("error: unexpected kind of JSON patch operation: \"{other}\"")),
        };
        if let Err(why) = result {
            return fail(&why);
        }
    }
    Ok(())
}

/// The tokens of a JSON pointer (`/a/b~1c` -> `["a", "b/c"]`).
fn tokens(path: &str) -> Vec<String> {
    path.split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn pointer<'a>(target: &'a Value, path: &str) -> Option<&'a Value> {
    tokens(path).iter().try_fold(target, |value, token| match value {
        Value::Object(fields) => fields.get(token),
        Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    })
}

/// The container a path's last token lives in, and that token.
fn parent<'a>(target: &'a mut Value, path: &str) -> Result<(&'a mut Value, String), String> {
    let mut tokens = tokens(path);
    let last = tokens
        .pop()
        .ok_or_else(|| "cannot operate on the whole document".to_string())?;
    let mut value = target;
    for token in &tokens {
        value = match value {
            Value::Object(fields) => fields.get_mut(token),
            Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
            _ => None,
        }
        .ok_or_else(|| format!("doc is missing path: {path}"))?;
    }
    Ok((value, last))
}

fn add(target: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let (container, last) = parent(target, path)?;
    match container {
        Value::Object(fields) => {
            fields.insert(last, value);
            Ok(())
        }
        Value::Array(items) => {
            let index = if last == "-" {
                items.len()
            } else {
                last.parse::<usize>().map_err(|_| format!("invalid index: {last}"))?
            };
            if index > items.len() {
                return Err(format!("index out of bounds: {path}"));
            }
            items.insert(index, value);
            Ok(())
        }
        _ => Err(format!("doc is missing path: {path}")),
    }
}

fn remove(target: &mut Value, path: &str) -> Result<Value, String> {
    let (container, last) = parent(target, path)?;
    let missing = || format!("doc is missing key: {path}");
    match container {
        Value::Object(fields) => fields.remove(&last).ok_or_else(missing),
        Value::Array(items) => match last.parse::<usize>() {
            Ok(i) if i < items.len() => Ok(items.remove(i)),
            _ => Err(missing()),
        },
        _ => Err(missing()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_replaces_lists_and_null_removes() {
        let mut doc = json!({"a": {"b": 1, "c": 2}, "list": [1, 2]});
        merge_patch(&mut doc, &json!({"a": {"b": null, "d": 3}, "list": [9]}));
        assert_eq!(doc, json!({"a": {"c": 2, "d": 3}, "list": [9]}));
    }

    #[test]
    fn strategic_merge_merges_containers_and_env_by_name() {
        let mut doc = json!({"spec": {"containers": [
            {"name": "app", "image": "app:1", "env": [{"name": "A", "value": "1"}, {"name": "B", "value": "2"}]},
            {"name": "side", "image": "side:1"}
        ]}});
        strategic_merge(
            &mut doc,
            &json!({"spec": {"containers": [
                {"name": "app", "env": [{"name": "B", "value": "two"}, {"name": "C", "value": "3"}, {"name": "A", "$patch": "delete"}]}
            ]}}),
        );
        assert_eq!(
            doc,
            json!({"spec": {"containers": [
                {"name": "app", "image": "app:1", "env": [{"name": "B", "value": "two"}, {"name": "C", "value": "3"}]},
                {"name": "side", "image": "side:1"}
            ]}})
        );
    }

    #[test]
    fn strategic_merge_picks_the_port_key_from_the_items() {
        let mut service = json!({"ports": [{"port": 80, "targetPort": 8080}, {"port": 443}]});
        strategic_merge(&mut service, &json!({"ports": [{"port": 80, "targetPort": 3000}]}));
        assert_eq!(
            service["ports"],
            json!([{"port": 80, "targetPort": 3000}, {"port": 443}])
        );

        let mut container = json!({"ports": [{"containerPort": 80, "name": "http"}]});
        strategic_merge(
            &mut container,
            &json!({"ports": [{"containerPort": 80, "name": "web"}]}),
        );
        assert_eq!(container["ports"], json!([{"containerPort": 80, "name": "web"}]));
    }

    #[test]
    fn json_patch_operations() {
        let mut doc = json!({"spec": {"replicas": 1, "list": ["a", "c"], "x/y": 1}});
        json_patch(
            &mut doc,
            &json!([
                {"op": "replace", "path": "/spec/replicas", "value": 3},
                {"op": "add", "path": "/spec/list/1", "value": "b"},
                {"op": "add", "path": "/spec/list/-", "value": "d"},
                {"op": "remove", "path": "/spec/x~1y"},
                {"op": "copy", "from": "/spec/replicas", "path": "/spec/copied"},
                {"op": "test", "path": "/spec/copied", "value": 3}
            ]),
        )
        .unwrap();
        assert_eq!(
            doc,
            json!({"spec": {"replicas": 3, "list": ["a", "b", "c", "d"], "copied": 3}})
        );

        let err = json_patch(&mut doc, &json!([{"op": "replace", "path": "/spec/nope", "value": 1}])).unwrap_err();
        assert_eq!(
            err,
            "Error from server: jsonpatch replace operation does not apply: doc is missing key: /spec/nope"
        );
        assert!(json_patch(&mut doc, &json!({"op": "add"})).is_err());
        let err = json_patch(&mut doc, &json!([{"op": "test", "path": "/spec/replicas", "value": 9}])).unwrap_err();
        assert_eq!(err, "Error from server: testing value /spec/replicas failed");
    }
}
