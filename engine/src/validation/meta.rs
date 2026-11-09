//! `k8s.io/apimachinery/pkg/api/validation` (ObjectMeta) and
//! `k8s.io/apimachinery/pkg/apis/meta/v1/validation` (labels, label selectors).

use serde_json::{Map, Value};

use super::field::{forbidden, invalid, required, too_long, Bad, ErrorList, Path};
use super::names::{is_qualified_name, is_valid_label_value, NameRule};
use super::{arr, i64_of, obj, str_of};

const TOTAL_ANNOTATION_SIZE_LIMIT: usize = 256 * 1024;
pub const NEGATIVE: &str = "must be greater than or equal to 0";
pub const IMMUTABLE: &str = "field is immutable";

/// `ValidateNonnegativeField`.
pub fn nonnegative(value: i64, path: &Path) -> ErrorList {
    if value < 0 {
        vec![invalid(path, Bad::int(value), NEGATIVE)]
    } else {
        Vec::new()
    }
}

/// `ValidateLabelName`.
pub fn label_name(name: &str, path: &Path) -> ErrorList {
    is_qualified_name(name)
        .into_iter()
        .map(|msg| invalid(path, Bad::str(name), &msg))
        .collect()
}

/// `ValidateLabels` (sorted keys: Go iterates its map in random order).
pub fn labels(labels: Option<&Map<String, Value>>, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for (k, v) in labels.into_iter().flatten() {
        errs.extend(label_name(k, path));
        let v = v.as_str().unwrap_or_default();
        for msg in is_valid_label_value(v) {
            errs.push(invalid(path, Bad::str(v), &msg));
        }
    }
    errs
}

/// `ValidateAnnotations`.
pub fn annotations(annotations: Option<&Map<String, Value>>, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut size = 0;
    for (k, v) in annotations.into_iter().flatten() {
        for msg in is_qualified_name(&k.to_lowercase()) {
            errs.push(invalid(path, Bad::str(k), &msg));
        }
        size += k.len() + v.as_str().map_or(0, str::len);
    }
    if size > TOTAL_ANNOTATION_SIZE_LIMIT {
        errs.push(too_long(path, TOTAL_ANNOTATION_SIZE_LIMIT));
    }
    errs
}

/// `ValidateFinalizerName`.
pub fn finalizer_name(name: &str, path: &Path) -> ErrorList {
    is_qualified_name(name)
        .into_iter()
        .map(|msg| invalid(path, Bad::str(name), &msg))
        .collect()
}

/// `ValidateObjectMeta` / `ValidateObjectMetaAccessor`: names, namespace, labels,
/// annotations, owner references, finalizers.
pub fn object_meta(meta: &Value, namespaced: bool, rule: NameRule) -> ErrorList {
    let path = Path::new("metadata");
    let mut errs = Vec::new();
    let generate_name = str_of(meta, "generateName");
    if !generate_name.is_empty() {
        for msg in rule.check(generate_name, true) {
            errs.push(invalid(&path.child("generateName"), Bad::str(generate_name), &msg));
        }
    }
    let name = str_of(meta, "name");
    if name.is_empty() {
        errs.push(required(&path.child("name"), "name or generateName is required"));
    } else {
        for msg in rule.check(name, false) {
            errs.push(invalid(&path.child("name"), Bad::str(name), &msg));
        }
    }
    errs.extend(object_meta_common(meta, namespaced, &path));
    errs
}

fn object_meta_common(meta: &Value, namespaced: bool, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let namespace = str_of(meta, "namespace");
    if namespaced {
        if namespace.is_empty() {
            errs.push(required(&path.child("namespace"), ""));
        } else {
            for msg in NameRule::Label.check(namespace, false) {
                errs.push(invalid(&path.child("namespace"), Bad::str(namespace), &msg));
            }
        }
    } else if !namespace.is_empty() {
        errs.push(forbidden(&path.child("namespace"), "not allowed on this type"));
    }
    errs.extend(nonnegative(i64_of(meta, "generation"), &path.child("generation")));
    errs.extend(labels(obj(meta, "labels"), &path.child("labels")));
    errs.extend(annotations(obj(meta, "annotations"), &path.child("annotations")));
    errs.extend(owner_references(
        arr(meta, "ownerReferences"),
        &path.child("ownerReferences"),
    ));
    for f in arr(meta, "finalizers") {
        errs.extend(finalizer_name(
            f.as_str().unwrap_or_default(),
            &path.child("finalizers"),
        ));
    }
    errs
}

fn owner_references(refs: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for r in refs {
        let api_version = str_of(r, "apiVersion");
        let version = api_version.rsplit('/').next().unwrap_or_default();
        if version.is_empty() {
            errs.push(invalid(
                &path.child("apiVersion"),
                Bad::str(api_version),
                "version must not be empty",
            ));
        }
        if str_of(r, "kind").is_empty() {
            errs.push(invalid(&path.child("kind"), Bad::str(""), "must not be empty"));
        }
        if str_of(r, "name").is_empty() {
            errs.push(invalid(&path.child("name"), Bad::str(""), "must not be empty"));
        }
        if str_of(r, "uid").is_empty() {
            errs.push(invalid(&path.child("uid"), Bad::str(""), "must not be empty"));
        }
    }
    errs
}

/// `ValidateObjectMetaAccessorUpdate`.
pub fn object_meta_update(new: &Value, old: &Value) -> ErrorList {
    let path = Path::new("metadata");
    let mut errs = Vec::new();
    if str_of(new, "resourceVersion").is_empty() {
        errs.push(invalid(
            &path.child("resourceVersion"),
            Bad::str(""),
            "must be specified for an update",
        ));
    }
    if i64_of(new, "generation") < i64_of(old, "generation") {
        errs.push(invalid(
            &path.child("generation"),
            Bad::int(i64_of(new, "generation")),
            "must not be decremented",
        ));
    }
    for key in ["name", "namespace", "uid"] {
        let (n, o) = (str_of(new, key), str_of(old, key));
        if n != o {
            errs.push(invalid(&path.child(key), Bad::str(n), IMMUTABLE));
        }
    }
    errs.extend(labels(obj(new, "labels"), &path.child("labels")));
    errs.extend(annotations(obj(new, "annotations"), &path.child("annotations")));
    errs.extend(owner_references(
        arr(new, "ownerReferences"),
        &path.child("ownerReferences"),
    ));
    errs
}

// ---- label selectors ----

/// `ValidateLabelSelector` (with `AllowInvalidLabelValueInSelector: false`).
pub fn label_selector(selector: &Value, path: &Path) -> ErrorList {
    let mut errs = labels(obj(selector, "matchLabels"), &path.child("matchLabels"));
    for (i, expr) in arr(selector, "matchExpressions").iter().enumerate() {
        errs.extend(selector_requirement(expr, &path.child("matchExpressions").index(i)));
    }
    errs
}

fn selector_requirement(sr: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let values = arr(sr, "values");
    let operator = str_of(sr, "operator");
    match operator {
        "In" | "NotIn" => {
            if values.is_empty() {
                errs.push(required(
                    &path.child("values"),
                    "must be specified when `operator` is 'In' or 'NotIn'",
                ));
            }
        }
        "Exists" | "DoesNotExist" => {
            if !values.is_empty() {
                errs.push(forbidden(
                    &path.child("values"),
                    "may not be specified when `operator` is 'Exists' or 'DoesNotExist'",
                ));
            }
        }
        _ => errs.push(invalid(
            &path.child("operator"),
            Bad::str(operator),
            "not a valid selector operator",
        )),
    }
    errs.extend(label_name(str_of(sr, "key"), &path.child("key")));
    for (i, v) in values.iter().enumerate() {
        let v = v.as_str().unwrap_or_default();
        for msg in is_valid_label_value(v) {
            errs.push(invalid(&path.child("values").index(i), Bad::str(v), &msg));
        }
    }
    errs
}

/// Whether `metav1.LabelSelectorAsSelector` accepts the selector.
pub fn selector_converts(selector: &Value) -> bool {
    label_selector(selector, &Path::new("")).is_empty()
}

/// Whether the selector is empty (matches everything), and otherwise whether it matches.
pub fn selector_matches(selector: &Value, labels: Option<&Map<String, Value>>) -> Option<bool> {
    let match_labels = obj(selector, "matchLabels");
    let exprs = arr(selector, "matchExpressions");
    if match_labels.is_none_or(Map::is_empty) && exprs.is_empty() {
        return None;
    }
    let get = |k: &str| labels.and_then(|l| l.get(k)).and_then(Value::as_str);
    let mut ok = match_labels.into_iter().flatten().all(|(k, v)| get(k) == v.as_str());
    for e in exprs {
        let key = str_of(e, "key");
        let values: Vec<&str> = arr(e, "values").iter().filter_map(Value::as_str).collect();
        ok &= match str_of(e, "operator") {
            "In" => get(key).is_some_and(|v| values.contains(&v)),
            "NotIn" => get(key).is_none_or(|v| !values.contains(&v)),
            "Exists" => get(key).is_some(),
            "DoesNotExist" => get(key).is_none(),
            _ => false,
        };
    }
    Some(ok)
}
