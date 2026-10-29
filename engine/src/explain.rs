//! `kubectl explain pod.spec.containers`: field documentation straight from the Kubernetes API
//! schemas that `k8s-openapi` ships, so descriptions are the real ones.

use k8s_openapi::schemars::SchemaGenerator;
use serde_json::Value;

use crate::api::{self, KindInfo};

/// The wrap width of descriptions, after their indent.
const WIDTH: usize = 76;

/// The JSON schema of a kind: the object schema plus a `$defs` table for everything it names.
fn root_schema(kind: &str) -> Option<Value> {
    use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
    use k8s_openapi::api::core::v1::{ConfigMap, Endpoints, Event, Namespace, Node, Pod, Secret, Service};
    let generator = SchemaGenerator::default();
    let schema = match kind {
        "ConfigMap" => generator.into_root_schema_for::<ConfigMap>(),
        "Deployment" => generator.into_root_schema_for::<Deployment>(),
        "Event" => generator.into_root_schema_for::<Event>(),
        "Namespace" => generator.into_root_schema_for::<Namespace>(),
        "Node" => generator.into_root_schema_for::<Node>(),
        "Pod" => generator.into_root_schema_for::<Pod>(),
        "ReplicaSet" => generator.into_root_schema_for::<ReplicaSet>(),
        "Secret" => generator.into_root_schema_for::<Secret>(),
        "Endpoints" => generator.into_root_schema_for::<Endpoints>(),
        "EndpointSlice" => generator.into_root_schema_for::<k8s_openapi::api::discovery::v1::EndpointSlice>(),
        "Service" => generator.into_root_schema_for::<Service>(),
        _ => return None,
    };
    Some(schema.to_value())
}

/// Follow a `$ref` to its definition. The referring schema's own `description` (a field's
/// documentation) wins over the definition's.
fn resolve<'a>(schema: &'a Value, defs: &'a Value) -> (&'a Value, Option<&'a str>) {
    let own = schema.get("description").and_then(Value::as_str);
    match schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.rsplit('/').next())
    {
        Some(name) => {
            let target = defs.get(name).unwrap_or(schema);
            (
                target,
                own.or_else(|| target.get("description").and_then(Value::as_str)),
            )
        }
        None => (schema, own),
    }
}

/// The name kubectl shows for a type: `string`, `[]Container`, `map[string]string`, `PodSpec`.
fn type_name(schema: &Value, defs: &Value) -> String {
    let (resolved, _) = resolve(schema, defs);
    match resolved.get("type").and_then(Value::as_str) {
        Some("array") => format!(
            "[]{}",
            resolved.get("items").map_or("Object".into(), |i| type_name(i, defs))
        ),
        Some("object") | None => {
            if let Some(values) = resolved.get("additionalProperties").filter(|v| v.is_object()) {
                if resolved.get("properties").is_none() {
                    return format!("map[string]{}", type_name(values, defs));
                }
            }
            schema
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.rsplit('.').next())
                .unwrap_or("Object")
                .to_string()
        }
        Some(primitive) => primitive.to_string(),
    }
}

/// The schema one step into an object field: through arrays to their element type.
fn element<'a>(schema: &'a Value, defs: &'a Value) -> &'a Value {
    let (resolved, _) = resolve(schema, defs);
    match (resolved.get("type").and_then(Value::as_str), resolved.get("items")) {
        (Some("array"), Some(items)) => element(items, defs),
        _ => schema,
    }
}

fn wrap(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut lines = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > WIDTH {
                lines.push(format!("{pad}{line}"));
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(format!("{pad}{line}"));
        }
        lines.push(String::new());
    }
    lines.pop();
    lines.join("\n")
}

fn header(info: &KindInfo) -> String {
    let mut out = String::new();
    if !info.group.is_empty() {
        out.push_str(&format!("GROUP:      {}\n", info.group));
    }
    let version = info.api_version.rsplit('/').next().unwrap_or(info.api_version);
    out.push_str(&format!("KIND:       {}\nVERSION:    {version}\n\n", info.kind));
    out
}

/// Document `RESOURCE[.FIELD...]`. `Err` is a message for `error: ...`.
pub fn explain(path: &str) -> Result<String, String> {
    let mut segments = path.split('.');
    let resource = segments.next().unwrap_or_default();
    let info = api::kind_for_arg(resource).ok_or_else(|| {
        if api::is_unsimulated_resource(resource) {
            format!("the resource type {resource} is not available in this terminal")
        } else {
            format!("the server doesn't have a resource type \"{resource}\"")
        }
    })?;
    let root = root_schema(info.kind).ok_or_else(|| format!("no documentation is available for {}", info.kind))?;
    let defs = &root["$defs"];

    let mut schema = &root;
    let mut walked = Vec::new();
    let mut description = root.get("description").and_then(Value::as_str);
    let mut field_type = None;
    for field in segments {
        let (object, _) = resolve(element(schema, defs), defs);
        let property = object
            .get("properties")
            .and_then(|p| p.get(field))
            .ok_or_else(|| format!("field \"{field}\" does not exist"))?;
        walked.push(field);
        field_type = Some(type_name(property, defs));
        description = resolve(property, defs).1;
        schema = property;
    }

    let mut out = header(info);
    if let Some(name) = walked.last() {
        out.push_str(&format!("FIELD: {name} <{}>\n\n", field_type.unwrap_or_default()));
    }
    if let Some(text) = description.filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!("DESCRIPTION:\n{}\n\n", wrap(text, 4)));
    }

    let (object, _) = resolve(element(schema, defs), defs);
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        out.push_str("FIELDS:\n");
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();
        let blocks: Vec<String> = names
            .into_iter()
            .map(|name| {
                let property = &properties[name];
                let marker = if required.contains(&name.as_str()) {
                    " -required-"
                } else {
                    ""
                };
                let mut block = format!("  {name}\t<{}>{marker}", type_name(property, defs));
                if let Some(text) = resolve(property, defs).1.filter(|d| !d.trim().is_empty()) {
                    block.push_str(&format!("\n{}", wrap(text, 4)));
                }
                block
            })
            .collect();
        out.push_str(&blocks.join("\n\n"));
    }
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resource_alone_lists_its_top_level_fields() {
        let out = explain("pods").unwrap();
        assert!(out.starts_with("KIND:       Pod\nVERSION:    v1\n\nDESCRIPTION:\n    Pod is a collection of containers that can run on a host."), "{out}");
        assert!(
            out.contains("\nFIELDS:\n  apiVersion\t<string>\n    APIVersion defines the versioned schema"),
            "{out}"
        );
        assert!(out.contains("\n  metadata\t<ObjectMeta> -required-\n"), "{out}");
        assert!(
            out.contains("\n  spec\t<PodSpec>\n    Specification of the desired behavior of the pod."),
            "{out}"
        );
    }

    #[test]
    fn groups_are_shown_for_grouped_kinds() {
        let out = explain("deploy").unwrap();
        assert!(
            out.starts_with("GROUP:      apps\nKIND:       Deployment\nVERSION:    v1\n"),
            "{out}"
        );
    }

    #[test]
    fn fields_are_found_through_objects_and_lists() {
        let out = explain("pod.spec.containers").unwrap();
        assert!(
            out.contains(
                "\nFIELD: containers <[]Container>\n\nDESCRIPTION:\n    List of containers belonging to the pod."
            ),
            "{out}"
        );
        assert!(out.contains("\n  image\t<string>\n"), "{out}");
        assert!(out.contains("\n  name\t<string> -required-\n"), "{out}");
        assert!(out.contains("\n  env\t<[]EnvVar>\n"), "{out}");

        let env = explain("deployment.spec.template.spec.containers.env").unwrap();
        assert!(env.contains("FIELD: env <[]EnvVar>"), "{env}");
        assert!(env.contains("\n  valueFrom\t<EnvVarSource>\n"), "{env}");
    }

    #[test]
    fn scalar_and_map_fields() {
        let out = explain("pod.spec.restartPolicy").unwrap();
        assert!(out.starts_with("KIND:       Pod\nVERSION:    v1\n\nFIELD: restartPolicy <string>\n\nDESCRIPTION:\n    Restart policy for all containers within the pod."), "{out}");
        assert!(!out.contains("FIELDS:"), "{out}");
        assert!(explain("pod.metadata.labels")
            .unwrap()
            .contains("FIELD: labels <map[string]string>"));
        assert!(explain("svc.spec.ports.port")
            .unwrap()
            .contains("FIELD: port <integer>"));
    }

    #[test]
    fn every_kind_can_be_explained() {
        for info in api::kinds() {
            let out = explain(&info.kind.to_lowercase()).unwrap();
            assert!(out.contains(&format!("KIND:       {}", info.kind)), "{}", info.kind);
            assert!(out.contains("\nFIELDS:\n"), "{}", info.kind);
        }
    }

    #[test]
    fn descriptions_wrap() {
        // A line may only be longer than the width when it is a single unbreakable word (a URL).
        for kind in ["pod.spec.restartPolicy", "pod", "deployment.spec.strategy"] {
            for line in explain(kind).unwrap().lines() {
                assert!(
                    line.len() <= WIDTH + 4 || line.split_whitespace().count() == 1,
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn errors_name_the_problem() {
        assert_eq!(
            explain("widgets").unwrap_err(),
            "the server doesn't have a resource type \"widgets\""
        );
        assert_eq!(explain("pod.spec.nope").unwrap_err(), "field \"nope\" does not exist");
        assert_eq!(
            explain("pod.metadata.labels.x").unwrap_err(),
            "field \"x\" does not exist"
        );
        assert_eq!(
            explain("pod.spec.restartPolicy.x").unwrap_err(),
            "field \"x\" does not exist"
        );
    }
}
