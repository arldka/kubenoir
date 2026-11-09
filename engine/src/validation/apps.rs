//! `k8s.io/kubernetes/pkg/apis/apps/validation` for Deployment and ReplicaSet.

use serde_json::{json, Value};

use super::core::pod_template_spec;
use super::field::{forbidden, go_json, invalid, not_supported, required, Bad, ErrorList, Path};
use super::meta::{self, nonnegative, IMMUTABLE};
use super::names::{is_valid_percent, NameRule};
use super::{has, i64_of, obj, opt_i64, str_of};

pub fn validate_deployment(d: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&d["metadata"], true, NameRule::Subdomain);
    errs.extend(deployment_spec(&d["spec"], &Path::new("spec")));
    errs
}

pub fn validate_deployment_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(deployment_spec(&new["spec"], &Path::new("spec")));
    errs.extend(immutable_selector(new, old));
    errs
}

pub fn validate_replica_set(rs: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&rs["metadata"], true, NameRule::Subdomain);
    errs.extend(replica_set_spec(&rs["spec"], &Path::new("spec")));
    errs
}

pub fn validate_replica_set_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(replica_set_spec(&new["spec"], &Path::new("spec")));
    errs.extend(immutable_selector(new, old));
    errs
}

fn immutable_selector(new: &Value, old: &Value) -> ErrorList {
    let (n, o) = (&new["spec"]["selector"], &old["spec"]["selector"]);
    if n != o {
        vec![invalid(
            &Path::new("spec").child("selector"),
            selector_bad(n),
            IMMUTABLE,
        )]
    } else {
        Vec::new()
    }
}

/// A label selector as a bad value: its JSON, fields in Go's order.
pub fn selector_bad(selector: &Value) -> Bad {
    if selector.is_null() {
        return Bad::Json("null".into());
    }
    let mut parts = Vec::new();
    if let Some(labels) = obj(selector, "matchLabels").filter(|m| !m.is_empty()) {
        parts.push(format!("\"matchLabels\":{}", go_json(&Value::Object(labels.clone()))));
    }
    if let Some(exprs) = selector
        .get("matchExpressions")
        .and_then(Value::as_array)
        .filter(|e| !e.is_empty())
    {
        let items: Vec<String> = exprs
            .iter()
            .map(|e| {
                let mut fields = vec![
                    format!("\"key\":{}", go_json(&json!(str_of(e, "key")))),
                    format!("\"operator\":{}", go_json(&json!(str_of(e, "operator")))),
                ];
                if let Some(values) = e.get("values").and_then(Value::as_array).filter(|v| !v.is_empty()) {
                    fields.push(format!("\"values\":{}", go_json(&Value::Array(values.clone()))));
                }
                format!("{{{}}}", fields.join(","))
            })
            .collect();
        parts.push(format!("\"matchExpressions\":[{}]", items.join(",")));
    }
    Bad::Json(format!("{{{}}}", parts.join(",")))
}

/// The selector checks `ValidateDeploymentSpec` and `ValidateReplicaSetSpec` share, then the
/// template (`ValidatePodTemplateSpecForReplicaSet`) when the selector converts.
fn selector_and_template(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let selector = &spec["selector"];
    if !has(spec, "selector") {
        errs.push(required(&path.child("selector"), ""));
    } else {
        errs.extend(meta::label_selector(selector, &path.child("selector")));
        let empty = obj(selector, "matchLabels").is_none_or(|m| m.is_empty())
            && selector["matchExpressions"].as_array().is_none_or(Vec::is_empty);
        if empty {
            errs.push(invalid(
                &path.child("selector"),
                selector_bad(selector),
                "empty selector is invalid for deployment",
            ));
        }
    }
    if has(spec, "selector") && !meta::selector_converts(selector) {
        errs.push(invalid(
            &path.child("selector"),
            selector_bad(selector),
            "invalid label selector",
        ));
    } else {
        errs.extend(template_for_replica_set(spec, path));
    }
    errs
}

fn template_for_replica_set(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let template = &spec["template"];
    let tpath = path.child("template");
    let labels = obj(&template["metadata"], "labels");
    if meta::selector_matches(&spec["selector"], labels) == Some(false) {
        let bad = match labels {
            Some(l) => Bad::Json(go_json(&Value::Object(l.clone()))),
            None => Bad::Json("null".into()),
        };
        errs.push(invalid(
            &tpath.child("metadata").child("labels"),
            bad,
            "`selector` does not match template `labels`",
        ));
    }
    errs.extend(pod_template_spec(template, &tpath));
    let restart = str_of(&template["spec"], "restartPolicy");
    if restart != "Always" {
        errs.push(not_supported(
            &tpath.child("spec").child("restartPolicy"),
            Bad::str(restart),
            &["Always"],
        ));
    }
    if has(&template["spec"], "activeDeadlineSeconds") {
        errs.push(forbidden(
            &tpath.child("spec").child("activeDeadlineSeconds"),
            "activeDeadlineSeconds in ReplicaSet is not Supported",
        ));
    }
    errs
}

fn replica_set_spec(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = nonnegative(i64_of(spec, "replicas"), &path.child("replicas"));
    errs.extend(nonnegative(
        i64_of(spec, "minReadySeconds"),
        &path.child("minReadySeconds"),
    ));
    errs.extend(selector_and_template(spec, path));
    errs
}

/// `ValidateDeploymentSpec`.
fn deployment_spec(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = nonnegative(i64_of(spec, "replicas"), &path.child("replicas"));
    errs.extend(selector_and_template(spec, path));
    errs.extend(deployment_strategy(&spec["strategy"], &path.child("strategy")));
    errs.extend(nonnegative(
        i64_of(spec, "minReadySeconds"),
        &path.child("minReadySeconds"),
    ));
    if let Some(limit) = opt_i64(spec, "revisionHistoryLimit") {
        errs.extend(nonnegative(limit, &path.child("revisionHistoryLimit")));
    }
    if let Some(deadline) = opt_i64(spec, "progressDeadlineSeconds") {
        errs.extend(nonnegative(deadline, &path.child("progressDeadlineSeconds")));
        if deadline <= i64_of(spec, "minReadySeconds") {
            errs.push(invalid(
                &path.child("progressDeadlineSeconds"),
                Bad::int(deadline),
                "must be greater than minReadySeconds",
            ));
        }
    }
    errs
}

fn deployment_strategy(strategy: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    match str_of(strategy, "type") {
        "Recreate" => {
            if has(strategy, "rollingUpdate") {
                errs.push(forbidden(
                    &path.child("rollingUpdate"),
                    "may not be specified when strategy `type` is 'Recreate'",
                ));
            }
        }
        "RollingUpdate" => match strategy.get("rollingUpdate").filter(|r| !r.is_null()) {
            None => errs.push(required(
                &path.child("rollingUpdate"),
                "this should be defaulted and never be nil",
            )),
            Some(r) => errs.extend(rolling_update(r, &path.child("rollingUpdate"))),
        },
        other => {
            let rolling = match strategy.get("rollingUpdate").filter(|r| !r.is_null()) {
                Some(_) => "{}",
                None => "null",
            };
            errs.push(not_supported(
                path,
                Bad::Json(format!(
                    "{{\"Type\":{},\"RollingUpdate\":{rolling}}}",
                    go_json(&json!(other))
                )),
                &["Recreate", "RollingUpdate"],
            ));
        }
    }
    errs
}

/// An IntOrString as a bad value: its JSON.
fn int_or_string_bad(v: &Value) -> Bad {
    Bad::Json(go_json(v))
}

/// `getIntOrPercentValue`.
fn int_or_percent_value(v: &Value) -> i64 {
    match v {
        Value::String(s) if is_valid_percent(s).is_empty() => s[..s.len() - 1].parse().unwrap_or_default(),
        Value::String(_) => 0,
        other => other.as_i64().unwrap_or_default(),
    }
}

fn positive_int_or_percent(v: &Value, path: &Path) -> ErrorList {
    match v {
        Value::String(s) => is_valid_percent(s)
            .into_iter()
            .map(|m| invalid(path, int_or_string_bad(v), &m))
            .collect(),
        other => nonnegative(other.as_i64().unwrap_or_default(), path),
    }
}

/// `ValidateRollingUpdateDeployment`.
fn rolling_update(r: &Value, path: &Path) -> ErrorList {
    let (max_unavailable, max_surge) = (&r["maxUnavailable"], &r["maxSurge"]);
    let mut errs = positive_int_or_percent(max_unavailable, &path.child("maxUnavailable"));
    errs.extend(positive_int_or_percent(max_surge, &path.child("maxSurge")));
    if int_or_percent_value(max_unavailable) == 0 && int_or_percent_value(max_surge) == 0 {
        errs.push(invalid(
            &path.child("maxUnavailable"),
            int_or_string_bad(max_unavailable),
            "may not be 0 when `maxSurge` is 0",
        ));
    }
    if let Value::String(s) = max_unavailable {
        if is_valid_percent(s).is_empty() && int_or_percent_value(max_unavailable) > 100 {
            errs.push(invalid(
                &path.child("maxUnavailable"),
                int_or_string_bad(max_unavailable),
                "must not be greater than 100%",
            ));
        }
    }
    errs
}
