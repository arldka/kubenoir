//! `k8s.io/kubernetes/pkg/apis/batch/validation` for Job and CronJob.

use serde_json::{json, Value};

use super::apps::selector_bad;
use super::core::pod_template_spec;
use super::field::{go_json, invalid, not_supported, required, Bad, ErrorList, Path};
use super::meta::{self, nonnegative};
use super::names::{NameRule, DNS1123_LABEL_MAX};
use super::{arr, bool_of, has, obj, opt_i64, str_of};

const LEGACY_CONTROLLER_UID: &str = "controller-uid";
const LEGACY_JOB_NAME: &str = "job-name";
const CONTROLLER_UID: &str = "batch.kubernetes.io/controller-uid";
const JOB_NAME: &str = "batch.kubernetes.io/job-name";

/// The Job strategy's `PrepareForCreate`: unless the selector is manual, the template gets the
/// job's name and uid as labels, and the selector the uid.
pub fn generate_selector(job: &mut Value) {
    if bool_of(&job["spec"], "manualSelector") {
        return;
    }
    let name = str_of(&job["metadata"], "name").to_string();
    let uid = str_of(&job["metadata"], "uid").to_string();
    let template_meta = &mut job["spec"]["template"]["metadata"];
    if !template_meta.is_object() {
        *template_meta = json!({});
    }
    let labels = template_meta
        .as_object_mut()
        .expect("object")
        .entry("labels")
        .or_insert_with(|| json!({}));
    if !labels.is_object() {
        *labels = json!({});
    }
    let labels = labels.as_object_mut().expect("object");
    for key in [LEGACY_JOB_NAME, JOB_NAME] {
        labels.entry(key).or_insert_with(|| json!(name));
    }
    for key in [LEGACY_CONTROLLER_UID, CONTROLLER_UID] {
        labels.entry(key).or_insert_with(|| json!(uid));
    }
    let selector = &mut job["spec"]["selector"];
    if !selector.is_object() {
        *selector = json!({});
    }
    let match_labels = selector
        .as_object_mut()
        .expect("object")
        .entry("matchLabels")
        .or_insert_with(|| json!({}));
    if !match_labels.is_object() {
        *match_labels = json!({});
    }
    match_labels
        .as_object_mut()
        .expect("object")
        .entry(CONTROLLER_UID)
        .or_insert_with(|| json!(uid));
}

pub fn validate_job(job: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&job["metadata"], true, NameRule::Subdomain);
    errs.extend(generated_selector(job));
    errs.extend(job_spec_with_selector(&job["spec"], &Path::new("spec")));
    let spec = &job["spec"];
    if str_of(spec, "completionMode") == "Indexed" {
        if let Some(completions) = opt_i64(spec, "completions").filter(|c| *c > 0) {
            let name = str_of(&job["metadata"], "name");
            let hostname = format!("{name}-{}", completions - 1);
            if !super::names::is_dns1123_label(&hostname).is_empty() {
                errs.push(invalid(
                    &Path::new("metadata").child("name"),
                    Bad::str(name),
                    &format!("will not able to create pod with invalid DNS label: {hostname}"),
                ));
            }
        }
    }
    errs
}

pub fn validate_job_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(validate_job(new));
    let path = Path::new("spec");
    if new["spec"]["selector"] != old["spec"]["selector"] {
        errs.push(invalid(
            &path.child("selector"),
            selector_bad(&new["spec"]["selector"]),
            meta::IMMUTABLE,
        ));
    }
    errs
}

/// `validateGeneratedSelector` (with the batch.kubernetes.io labels required).
fn generated_selector(job: &Value) -> ErrorList {
    let mut errs = Vec::new();
    let spec = &job["spec"];
    if bool_of(spec, "manualSelector") || !has(spec, "selector") {
        return errs;
    }
    let uid = str_of(&job["metadata"], "uid");
    let name = str_of(&job["metadata"], "name");
    if uid.is_empty() {
        errs.push(required(&Path::new("metadata").child("uid"), ""));
    }
    let tmeta = Path::new("spec").child("template").child("metadata");
    let labels = obj(&spec["template"]["metadata"], "labels");
    for (key, value) in [
        (LEGACY_CONTROLLER_UID, uid),
        (LEGACY_JOB_NAME, name),
        (CONTROLLER_UID, uid),
        (JOB_NAME, name),
    ] {
        // `ValidateHasLabel`.
        match labels.and_then(|l| l.get(key)).and_then(Value::as_str) {
            None => errs.push(required(&tmeta.child("labels").key(key), value)),
            Some(actual) if actual != value => errs.push(invalid(
                &tmeta.child("labels").key(key),
                Bad::Json(go_json(&Value::Object(labels.cloned().unwrap_or_default()))),
                &format!("must be '{value}'"),
            )),
            _ => {}
        }
    }
    let expected = json!({
        CONTROLLER_UID: uid,
        JOB_NAME: name,
        LEGACY_CONTROLLER_UID: uid,
        LEGACY_JOB_NAME: name,
    });
    let selector = &spec["selector"];
    if meta::selector_converts(selector) && meta::selector_matches(selector, expected.as_object()).is_some_and(|m| !m) {
        errs.push(invalid(
            &Path::new("spec").child("selector"),
            selector_bad(selector),
            "`selector` not auto-generated",
        ));
    }
    errs
}

/// `ValidateJobSpec`: the spec, then the selector and whether it matches the template.
fn job_spec_with_selector(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = job_spec(spec, path);
    let selector = &spec["selector"];
    if !has(spec, "selector") {
        errs.push(required(&path.child("selector"), ""));
    } else {
        errs.extend(meta::label_selector(selector, &path.child("selector")));
    }
    let labels = obj(&spec["template"]["metadata"], "labels");
    let matches = if has(spec, "selector") {
        meta::selector_converts(selector) && meta::selector_matches(selector, labels) != Some(false)
    } else {
        // A nil selector converts to one that matches nothing.
        false
    };
    if !matches && (has(spec, "selector") && meta::selector_converts(selector) || !has(spec, "selector")) {
        let bad = match labels {
            Some(l) => Bad::Json(go_json(&Value::Object(l.clone()))),
            None => Bad::Json("null".into()),
        };
        errs.push(invalid(
            &path.child("template").child("metadata").child("labels"),
            bad,
            "`selector` does not match template `labels`",
        ));
    }
    errs
}

/// `validateJobSpec`.
fn job_spec(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for key in [
        "parallelism",
        "completions",
        "activeDeadlineSeconds",
        "backoffLimit",
        "ttlSecondsAfterFinished",
        "backoffLimitPerIndex",
    ] {
        if let Some(v) = opt_i64(spec, key) {
            errs.extend(nonnegative(v, &path.child(key)));
        }
    }
    if let Some(v) = opt_i64(spec, "maxFailedIndexes") {
        errs.extend(nonnegative(v, &path.child("maxFailedIndexes")));
        if !has(spec, "backoffLimitPerIndex") {
            errs.push(required(
                &path.child("backoffLimitPerIndex"),
                "when maxFailedIndexes is specified",
            ));
        }
    }
    let mode = spec.get("completionMode").and_then(Value::as_str);
    if let Some(mode) = mode {
        if mode != "NonIndexed" && mode != "Indexed" {
            errs.push(not_supported(
                &path.child("completionMode"),
                Bad::Json(go_json(&json!(mode))),
                &["NonIndexed", "Indexed"],
            ));
        }
        if mode == "Indexed" && !has(spec, "completions") {
            errs.push(required(&path.child("completions"), "when completion mode is Indexed"));
        }
    }
    if mode.is_none_or(|m| m == "NonIndexed") {
        if let Some(v) = opt_i64(spec, "backoffLimitPerIndex") {
            errs.push(invalid(
                &path.child("backoffLimitPerIndex"),
                Bad::int(v),
                "requires indexed completion mode",
            ));
        }
        if let Some(v) = opt_i64(spec, "maxFailedIndexes") {
            errs.push(invalid(
                &path.child("maxFailedIndexes"),
                Bad::int(v),
                "requires indexed completion mode",
            ));
        }
    }
    if let Some(policy) = spec.get("podFailurePolicy").filter(|p| !p.is_null()) {
        for (i, rule) in arr(policy, "rules").iter().enumerate() {
            let rule_path = path.child("podFailurePolicy").child("rules").index(i);
            for (j, pattern) in arr(rule, "onPodConditions").iter().enumerate() {
                let p = rule_path.child("onPodConditions").index(j);
                let t = str_of(pattern, "type");
                for m in super::names::is_qualified_name(t) {
                    errs.push(invalid(&p.child("type"), Bad::str(t), &m));
                }
                match str_of(pattern, "status") {
                    "" => errs.push(required(
                        &p.child("status"),
                        "valid values: [\"False\" \"True\" \"Unknown\"]",
                    )),
                    "True" | "False" | "Unknown" => {}
                    other => errs.push(not_supported(
                        &p.child("status"),
                        Bad::str(other),
                        &["False", "True", "Unknown"],
                    )),
                }
            }
        }
    }
    if let Some(policy) = spec.get("podReplacementPolicy").and_then(Value::as_str) {
        if has(spec, "podFailurePolicy") {
            if policy != "Failed" {
                errs.push(not_supported(
                    &path.child("podReplacementPolicy"),
                    Bad::str(policy),
                    &["Failed"],
                ));
            }
        } else if policy != "Failed" && policy != "TerminatingOrFailed" {
            errs.push(not_supported(
                &path.child("podReplacementPolicy"),
                Bad::str(policy),
                &["Failed", "TerminatingOrFailed"],
            ));
        }
    }
    errs.extend(pod_template_spec(&spec["template"], &path.child("template")));
    let restart = str_of(&spec["template"]["spec"], "restartPolicy");
    let restart_path = path.child("template").child("spec").child("restartPolicy");
    if restart == "Always" || restart.is_empty() {
        errs.push(required(&restart_path, "valid values: \"OnFailure\", \"Never\""));
    } else if restart != "OnFailure" && restart != "Never" {
        errs.push(not_supported(&restart_path, Bad::str(restart), &["OnFailure", "Never"]));
    } else if has(spec, "podFailurePolicy") && restart != "Never" {
        errs.push(invalid(
            &restart_path,
            Bad::str(restart),
            "only \"Never\" is supported when podFailurePolicy is specified",
        ));
    }
    errs
}

pub fn validate_cron_job_create(cj: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&cj["metadata"], true, NameRule::Subdomain);
    errs.extend(cron_job_spec(&cj["spec"], None));
    let name = str_of(&cj["metadata"], "name");
    if name.len() > DNS1123_LABEL_MAX - 11 {
        errs.push(invalid(
            &Path::new("metadata").child("name"),
            Bad::str(name),
            "must be no more than 52 characters",
        ));
    }
    errs
}

pub fn validate_cron_job_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(cron_job_spec(&new["spec"], Some(&old["spec"])));
    errs
}

/// `validateCronJobSpec`.
fn cron_job_spec(spec: &Value, old: Option<&Value>) -> ErrorList {
    let path = Path::new("spec");
    let mut errs = Vec::new();
    let schedule = str_of(spec, "schedule");
    if schedule.is_empty() {
        errs.push(required(&path.child("schedule"), ""));
    } else {
        let allow_tz = old.is_some_and(|o| str_of(o, "schedule").contains("TZ"));
        if let Err(e) = crate::cron::parse(schedule) {
            errs.push(invalid(&path.child("schedule"), Bad::str(schedule), &e));
        }
        let time_zone = spec.get("timeZone").filter(|t| !t.is_null());
        if allow_tz && schedule.contains("TZ") && time_zone.is_some() {
            errs.push(invalid(
                &path.child("schedule"),
                Bad::str(schedule),
                "cannot use both timeZone field and TZ or CRON_TZ in schedule",
            ));
        } else if !allow_tz && schedule.contains("TZ") {
            errs.push(invalid(
                &path.child("schedule"),
                Bad::str(schedule),
                "cannot use TZ or CRON_TZ in schedule, use timeZone field instead",
            ));
        }
    }
    if let Some(v) = opt_i64(spec, "startingDeadlineSeconds") {
        errs.extend(nonnegative(v, &path.child("startingDeadlineSeconds")));
    }
    if old.is_none_or(|o| o.get("timeZone") != spec.get("timeZone")) {
        errs.extend(time_zone(
            spec.get("timeZone").and_then(Value::as_str),
            &path.child("timeZone"),
        ));
    }
    match str_of(spec, "concurrencyPolicy") {
        "Allow" | "Forbid" | "Replace" => {}
        "" => errs.push(required(&path.child("concurrencyPolicy"), "")),
        other => errs.push(not_supported(
            &path.child("concurrencyPolicy"),
            Bad::str(other),
            &["Allow", "Forbid", "Replace"],
        )),
    }
    let template_path = path.child("jobTemplate");
    let job = &spec["jobTemplate"]["spec"];
    errs.extend(job_spec(job, &template_path.child("spec")));
    if has(job, "selector") {
        errs.push(invalid(
            &template_path.child("spec").child("selector"),
            selector_bad(&job["selector"]),
            "`selector` will be auto-generated",
        ));
    }
    if bool_of(job, "manualSelector") {
        errs.push(not_supported(
            &template_path.child("spec").child("manualSelector"),
            Bad::Json("true".into()),
            &["nil", "false"],
        ));
    }
    for key in ["successfulJobsHistoryLimit", "failedJobsHistoryLimit"] {
        if let Some(v) = opt_i64(spec, key) {
            errs.extend(nonnegative(v, &path.child(key)));
        }
    }
    errs
}

/// `validateTimeZone`.
fn time_zone(tz: Option<&str>, path: &Path) -> ErrorList {
    let Some(tz) = tz else { return Vec::new() };
    let bad = || Bad::Json(go_json(&json!(tz)));
    if tz.is_empty() {
        return vec![invalid(path, bad(), "timeZone must be nil or non-empty string")];
    }
    let part_ok = |p: &str| {
        p != "."
            && p != ".."
            && !p.starts_with('-')
            && (1..=14).contains(&p.len())
            && p.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_+".contains(&c))
    };
    if !tz.split('/').all(part_ok) {
        return vec![invalid(path, bad(), &format!("unknown time zone {tz}"))];
    }
    let mut errs = Vec::new();
    if tz.eq_ignore_ascii_case("Local") {
        errs.push(invalid(
            path,
            bad(),
            "timeZone must be an explicit time zone as defined in https://www.iana.org/time-zones",
        ));
    }
    if !crate::tznames::is_known(tz) && tz != "Local" {
        errs.push(invalid(path, bad(), &format!("unknown time zone {tz}")));
    }
    errs
}
