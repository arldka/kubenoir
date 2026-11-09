//! `k8s.io/kubernetes/pkg/apis/core/validation` for Pod, Service, ConfigMap, Secret, Namespace
//! (and the metadata of the other core kinds).

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::field::{duplicate, forbidden, invalid, not_found, not_supported, required, too_long, Bad, ErrorList, Path};
use super::meta::{self, nonnegative, IMMUTABLE};
use super::names::{
    inclusive_range_error, is_config_map_key, is_dns1123_label, is_dns1123_subdomain, is_http_header_name,
    is_qualified_name, is_relaxed_env_var_name, is_valid_label_value, is_valid_port_name, is_valid_port_num,
    is_valid_user_id, NameRule,
};
use super::{arr, bool_of, has, i64_of, obj, opt_i64, str_of};
use crate::quantity::Quantity;

const FILE_MODE_ERR: &str = "must be a number between 0 and 0777 (octal), both inclusive";
const MAX_SECRET_SIZE: usize = 1024 * 1024;
const PROTOCOLS: &[&str] = &["SCTP", "TCP", "UDP"];

/// `ValidateDNS1123Label`.
fn dns1123_label(value: &str, path: &Path) -> ErrorList {
    is_dns1123_label(value)
        .into_iter()
        .map(|m| invalid(path, Bad::str(value), &m))
        .collect()
}

/// `ValidateDNS1123Subdomain`.
fn dns1123_subdomain(value: &str, path: &Path) -> ErrorList {
    is_dns1123_subdomain(value)
        .into_iter()
        .map(|m| invalid(path, Bad::str(value), &m))
        .collect()
}

/// `ValidateQualifiedName`.
fn qualified_name(value: &str, path: &Path) -> ErrorList {
    is_qualified_name(value)
        .into_iter()
        .map(|m| invalid(path, Bad::str(value), &m))
        .collect()
}

/// A name checked with a name rule, as `for _, msg := range nameFn(name, false)` does.
fn name_rule(rule: NameRule, value: &str, path: &Path) -> ErrorList {
    rule.check(value, false)
        .into_iter()
        .map(|m| invalid(path, Bad::str(value), &m))
        .collect()
}

fn file_mode(v: &Value, key: &str, path: &Path) -> ErrorList {
    match opt_i64(v, key) {
        Some(mode) if !(0..=0o777).contains(&mode) => vec![invalid(&path.child(key), Bad::int(mode), FILE_MODE_ERR)],
        _ => Vec::new(),
    }
}

// ---- pods ----

pub fn validate_pod_create(pod: &Value) -> ErrorList {
    let mut errs = pod_metadata_and_spec(pod);
    let spec_path = Path::new("spec");
    if !arr(&pod["spec"], "ephemeralContainers").is_empty() {
        errs.push(forbidden(
            &spec_path.child("ephemeralContainers"),
            "cannot be set on create",
        ));
    }
    errs
}

/// `validatePodMetadataAndSpec`.
fn pod_metadata_and_spec(pod: &Value) -> ErrorList {
    let spec = &pod["spec"];
    let spec_path = Path::new("spec");
    let mut errs = meta::object_meta(&pod["metadata"], true, NameRule::Subdomain);
    errs.extend(pod_spec(spec, &spec_path));
    if str_of(spec, "serviceAccountName").is_empty() {
        for (vi, volume) in arr(spec, "volumes").iter().enumerate() {
            let path = spec_path.child("volumes").index(vi).child("projected");
            for (si, source) in arr(&volume["projected"], "sources").iter().enumerate() {
                if has(source, "serviceAccountToken") {
                    errs.push(forbidden(
                        &path.child("sources").index(si).child("serviceAccountToken"),
                        "must not be specified when serviceAccountName is not set",
                    ));
                }
            }
        }
    }
    for list in ["containers", "initContainers"] {
        for (i, c) in arr(spec, list).iter().enumerate() {
            let image = str_of(c, "image");
            if image.len() != image.trim().len() {
                errs.push(invalid(
                    &spec_path.child(list).index(i).child("image"),
                    Bad::str(image),
                    "must not have leading or trailing whitespace",
                ));
            }
        }
    }
    errs
}

/// `ValidatePodUpdate`.
pub fn validate_pod_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(pod_metadata_and_spec(new));
    let spec_path = Path::new("spec");
    for list in ["containers", "initContainers"] {
        let (n, o) = (arr(&new["spec"], list), arr(&old["spec"], list));
        if n.len() != o.len() {
            errs.push(forbidden(
                &spec_path.child(list),
                "pod updates may not add or remove containers",
            ));
            return errs;
        }
        for (i, c) in n.iter().enumerate() {
            let image = str_of(c, "image");
            if image.is_empty() {
                errs.push(required(&spec_path.child(list).index(i).child("image"), ""));
            }
            if image.trim().len() != image.len() {
                errs.push(invalid(
                    &spec_path.child(list).index(i).child("image"),
                    Bad::str(image),
                    "must not have leading or trailing whitespace",
                ));
            }
        }
    }
    let (new_spec, old_spec) = (&new["spec"], &old["spec"]);
    match (
        opt_i64(new_spec, "activeDeadlineSeconds"),
        opt_i64(old_spec, "activeDeadlineSeconds"),
    ) {
        (Some(n), _) if !(0..=i32::MAX as i64).contains(&n) => {
            errs.push(invalid(
                &spec_path.child("activeDeadlineSeconds"),
                Bad::int(n),
                &inclusive_range_error(0, i32::MAX as i64),
            ));
            return errs;
        }
        (Some(n), Some(o)) if o < n => {
            errs.push(invalid(
                &spec_path.child("activeDeadlineSeconds"),
                Bad::int(n),
                "must be less than or equal to previous value",
            ));
            return errs;
        }
        (None, Some(_)) => errs.push(invalid(
            &spec_path.child("activeDeadlineSeconds"),
            Bad::Json("null".into()),
            "must not update from a positive integer to nil value",
        )),
        _ => {}
    }
    // Only additions to the tolerations.
    let (nt, ot) = (arr(new_spec, "tolerations"), arr(old_spec, "tolerations"));
    let kept = ot.iter().all(|t| nt.iter().any(|n| tolerations_equal(n, t)));
    if !kept {
        errs.push(forbidden(
            &spec_path.child("tolerations"),
            "existing toleration can not be modified except its tolerationSeconds",
        ));
    }
    if !pod_spec_change_allowed(old_spec, new_spec) {
        errs.push(forbidden(
            &spec_path,
            &format!(
                "pod updates may not change fields other than {}",
                UPDATABLE_POD_SPEC_FIELDS.join(",")
            ),
        ));
    }
    errs
}

fn tolerations_equal(a: &Value, b: &Value) -> bool {
    ["key", "operator", "value", "effect"]
        .iter()
        .all(|k| str_of(a, k) == str_of(b, k))
}

const UPDATABLE_POD_SPEC_FIELDS: &[&str] = &[
    "`spec.containers[*].image`",
    "`spec.initContainers[*].image`",
    "`spec.activeDeadlineSeconds`",
    "`spec.tolerations` (only additions to existing tolerations)",
    "`spec.terminationGracePeriodSeconds` (allow it to be set to 1 if it was previously negative)",
];

/// Whether `new` differs from `old` only where a running pod may change (the "munged" spec of
/// `ValidatePodUpdate` equal to the old one).
fn pod_spec_change_allowed(old: &Value, new: &Value) -> bool {
    let mut munged = new.clone();
    for list in ["containers", "initContainers"] {
        if let Some(items) = munged.get_mut(list).and_then(Value::as_array_mut) {
            for (i, c) in items.iter_mut().enumerate() {
                c["image"] = old[list][i]["image"].clone();
            }
        }
    }
    let fields = munged.as_object_mut().expect("pod spec is an object");
    for key in ["activeDeadlineSeconds", "schedulingGates", "tolerations"] {
        match old.get(key) {
            Some(v) => fields.insert(key.into(), v.clone()),
            None => fields.remove(key),
        };
    }
    if opt_i64(old, "terminationGracePeriodSeconds").is_some_and(|g| g < 0)
        && opt_i64(&munged, "terminationGracePeriodSeconds") == Some(1)
    {
        munged["terminationGracePeriodSeconds"] = old["terminationGracePeriodSeconds"].clone();
    }
    strip_nulls(&munged) == strip_nulls(old)
}

fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, x)| !x.is_null())
                .map(|(k, x)| (k.clone(), strip_nulls(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_nulls).collect()),
        other => other.clone(),
    }
}

/// `ValidatePodTemplateSpec` (the template at `path`, e.g. `spec.template`).
pub fn pod_template_spec(template: &Value, path: &Path) -> ErrorList {
    let meta = &template["metadata"];
    let mut errs = meta::labels(obj(meta, "labels"), &path.child("labels"));
    errs.extend(meta::annotations(obj(meta, "annotations"), &path.child("annotations")));
    errs.extend(pod_spec(&template["spec"], &path.child("spec")));
    if !arr(&template["spec"], "ephemeralContainers").is_empty() {
        errs.push(forbidden(
            &path.child("spec").child("ephemeralContainers"),
            "ephemeral containers not allowed in pod template",
        ));
    }
    errs
}

/// `ValidatePodSpec` (the parts the port covers, in upstream's order).
pub fn pod_spec(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    if !has(spec, "terminationGracePeriodSeconds") {
        errs.push(required(&path.child("terminationGracePeriodSeconds"), ""));
    }
    let (volumes, verrs) = validate_volumes(arr(spec, "volumes"), &path.child("volumes"));
    errs.extend(verrs);
    errs.extend(validate_containers(
        arr(spec, "containers"),
        &volumes,
        &path.child("containers"),
    ));
    errs.extend(validate_init_containers(
        arr(spec, "initContainers"),
        arr(spec, "containers"),
        &volumes,
        &path.child("initContainers"),
    ));
    errs.extend(restart_policy(
        str_of(spec, "restartPolicy"),
        &path.child("restartPolicy"),
    ));
    errs.extend(dns_policy(str_of(spec, "dnsPolicy"), &path.child("dnsPolicy")));
    errs.extend(meta::labels(obj(spec, "nodeSelector"), &path.child("nodeSelector")));
    errs.extend(pod_security_context(
        &spec["securityContext"],
        &path.child("securityContext"),
    ));
    for (i, s) in arr(spec, "imagePullSecrets").iter().enumerate() {
        if s.as_object().is_some_and(|m| m.keys().any(|k| k != "name")) {
            errs.push(invalid(
                &path.child("imagePullSecrets").index(i),
                Bad::Json(format!(
                    "{{\"Name\":{}}}",
                    super::field::go_json(&json!(str_of(s, "name")))
                )),
                "only name may be set",
            ));
        }
    }
    let sa = str_of(spec, "serviceAccountName");
    if !sa.is_empty() {
        errs.extend(name_rule(NameRule::Subdomain, sa, &path.child("serviceAccountName")));
    }
    let node = str_of(spec, "nodeName");
    if !node.is_empty() {
        errs.extend(name_rule(NameRule::Subdomain, node, &path.child("nodeName")));
    }
    if let Some(v) = opt_i64(spec, "activeDeadlineSeconds") {
        if !(1..=i32::MAX as i64).contains(&v) {
            errs.push(invalid(
                &path.child("activeDeadlineSeconds"),
                Bad::int(v),
                &inclusive_range_error(1, i32::MAX as i64),
            ));
        }
    }
    for key in ["hostname", "subdomain"] {
        let v = str_of(spec, key);
        if !v.is_empty() {
            errs.extend(dns1123_label(v, &path.child(key)));
        }
    }
    errs.extend(validate_tolerations(
        arr(spec, "tolerations"),
        &path.child("tolerations"),
    ));
    let priority_class = str_of(spec, "priorityClassName");
    if !priority_class.is_empty() {
        errs.extend(name_rule(
            NameRule::Subdomain,
            priority_class,
            &path.child("priorityClassName"),
        ));
    }
    if let Some(rc) = spec.get("runtimeClassName").and_then(Value::as_str) {
        errs.extend(name_rule(NameRule::Subdomain, rc, &path.child("runtimeClassName")));
    }
    if let Some(p) = spec.get("preemptionPolicy").and_then(Value::as_str) {
        match p {
            "PreemptLowerPriority" | "Never" => {}
            "" => errs.push(required(&path.child("preemptionPolicy"), "")),
            _ => errs.push(not_supported(
                &path.child("preemptionPolicy"),
                Bad::Json(super::field::go_json(&json!(p))),
                &["PreemptLowerPriority", "Never"],
            )),
        }
    }
    errs
}

fn restart_policy(policy: &str, path: &Path) -> ErrorList {
    match policy {
        "Always" | "OnFailure" | "Never" => Vec::new(),
        "" => vec![required(path, "")],
        _ => vec![not_supported(path, Bad::str(policy), &["Always", "OnFailure", "Never"])],
    }
}

fn dns_policy(policy: &str, path: &Path) -> ErrorList {
    match policy {
        "ClusterFirstWithHostNet" | "ClusterFirst" | "Default" | "None" => Vec::new(),
        "" => vec![required(path, "")],
        _ => vec![not_supported(
            path,
            Bad::Json(super::field::go_json(&json!(policy))),
            &["ClusterFirstWithHostNet", "ClusterFirst", "Default", "None"],
        )],
    }
}

fn pod_security_context(sc: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for key in ["runAsUser", "runAsGroup", "fsGroup"] {
        if let Some(id) = opt_i64(sc, key) {
            for msg in is_valid_user_id(id) {
                errs.push(invalid(&path.child(key), Bad::int(id), &msg));
            }
        }
    }
    for (i, g) in arr(sc, "supplementalGroups").iter().enumerate() {
        let id = g.as_i64().unwrap_or_default();
        for msg in is_valid_user_id(id) {
            errs.push(invalid(&path.child("supplementalGroups").index(i), Bad::int(id), &msg));
        }
    }
    errs
}

/// `ValidateVolumes`: the names of the valid volumes, and the errors.
fn validate_volumes(volumes: &[Value], path: &Path) -> (BTreeSet<String>, ErrorList) {
    let mut errs = Vec::new();
    let mut names = BTreeSet::new();
    for (i, vol) in volumes.iter().enumerate() {
        let idx = path.index(i);
        let name = str_of(vol, "name");
        let mut el = volume_source(vol, &idx);
        if name.is_empty() {
            el.push(required(&idx.child("name"), ""));
        } else {
            el.extend(dns1123_label(name, &idx.child("name")));
        }
        if names.contains(name) {
            el.push(duplicate(&idx.child("name"), Bad::str(name)));
        }
        if el.is_empty() {
            names.insert(name.to_string());
        } else {
            errs.extend(el);
        }
    }
    (names, errs)
}

/// The volume source fields, in the order `validateVolumeSource` checks them.
const VOLUME_SOURCES: &[&str] = &[
    "emptyDir",
    "hostPath",
    "gitRepo",
    "gcePersistentDisk",
    "awsElasticBlockStore",
    "secret",
    "nfs",
    "iscsi",
    "glusterfs",
    "flocker",
    "persistentVolumeClaim",
    "rbd",
    "cinder",
    "cephfs",
    "quobyte",
    "downwardAPI",
    "fc",
    "flexVolume",
    "configMap",
    "azureFile",
    "vsphereVolume",
    "photonPersistentDisk",
    "portworxVolume",
    "azureDisk",
    "storageos",
    "projected",
    "scaleIO",
    "csi",
    "ephemeral",
    "image",
];

const HOST_PATH_TYPES: &[&str] = &[
    "",
    "BlockDevice",
    "CharDevice",
    "Directory",
    "DirectoryOrCreate",
    "File",
    "FileOrCreate",
    "Socket",
];

/// `validateVolumeSource` for the sources a game pod uses; the others are only counted.
fn volume_source(vol: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut count = 0;
    for &source in VOLUME_SOURCES {
        if !has(vol, source) {
            continue;
        }
        let p = path.child(source);
        if source == "emptyDir" {
            count += 1;
            let negative = vol[source]
                .get("sizeLimit")
                .and_then(Value::as_str)
                .and_then(Quantity::parse)
                .is_some_and(|q| q.sign() < 0);
            if negative {
                errs.push(forbidden(
                    &p.child("sizeLimit"),
                    "SizeLimit field must be a valid resource quantity",
                ));
            }
            continue;
        }
        if count > 0 {
            errs.push(forbidden(&p, "may not specify more than 1 volume type"));
            continue;
        }
        count += 1;
        let s = &vol[source];
        match source {
            "hostPath" => {
                if str_of(s, "path").is_empty() {
                    errs.push(required(&p.child("path"), ""));
                } else {
                    errs.extend(path_no_backsteps(str_of(s, "path"), &p.child("path")));
                    if let Some(t) = s.get("type").and_then(Value::as_str) {
                        if !HOST_PATH_TYPES.contains(&t) {
                            errs.push(not_supported(
                                &p.child("type"),
                                Bad::Json(super::field::go_json(&json!(t))),
                                HOST_PATH_TYPES,
                            ));
                        }
                    }
                }
            }
            "secret" => {
                if str_of(s, "secretName").is_empty() {
                    errs.push(required(&p.child("secretName"), ""));
                }
                errs.extend(file_mode(s, "defaultMode", &p));
                for (i, kp) in arr(s, "items").iter().enumerate() {
                    errs.extend(key_to_path(kp, &p.child("items").index(i)));
                }
            }
            "nfs" => {
                if str_of(s, "server").is_empty() {
                    errs.push(required(&p.child("server"), ""));
                }
                if str_of(s, "path").is_empty() {
                    errs.push(required(&p.child("path"), ""));
                }
                if !str_of(s, "path").starts_with('/') {
                    errs.push(invalid(
                        &p.child("path"),
                        Bad::str(str_of(s, "path")),
                        "must be an absolute path",
                    ));
                }
            }
            "persistentVolumeClaim" => {
                if str_of(s, "claimName").is_empty() {
                    errs.push(required(&p.child("claimName"), ""));
                }
            }
            "downwardAPI" => errs.extend(file_mode(s, "defaultMode", &p)),
            "configMap" => {
                if str_of(s, "name").is_empty() {
                    errs.push(required(&p.child("name"), ""));
                }
                errs.extend(file_mode(s, "defaultMode", &p));
                for (i, kp) in arr(s, "items").iter().enumerate() {
                    errs.extend(key_to_path(kp, &p.child("items").index(i)));
                }
            }
            "projected" => errs.extend(file_mode(s, "defaultMode", &p)),
            _ => {}
        }
    }
    if count == 0 {
        errs.push(required(path, "must specify a volume type"));
    }
    errs
}

/// `validateKeyToPath`.
fn key_to_path(kp: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    if str_of(kp, "key").is_empty() {
        errs.push(required(&path.child("key"), ""));
    }
    let target = str_of(kp, "path");
    if target.is_empty() {
        errs.push(required(&path.child("path"), ""));
    }
    errs.extend(local_non_reserved_path(target, &path.child("path")));
    errs.extend(file_mode(kp, "mode", path));
    errs
}

fn path_no_backsteps(target: &str, path: &Path) -> ErrorList {
    if target.split('/').any(|p| p == "..") {
        vec![invalid(path, Bad::str(target), "must not contain '..'")]
    } else {
        Vec::new()
    }
}

/// `validateLocalDescendingPath`.
fn local_descending_path(target: &str, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    if target.starts_with('/') {
        errs.push(invalid(path, Bad::str(target), "must be a relative path"));
    }
    errs.extend(path_no_backsteps(target, path));
    errs
}

/// `ValidateLocalNonReservedPath`.
fn local_non_reserved_path(target: &str, path: &Path) -> ErrorList {
    let mut errs = local_descending_path(target, path);
    if target.starts_with("..") && !target.starts_with("../") {
        errs.push(invalid(path, Bad::str(target), "must not start with '..'"));
    }
    errs
}

/// `validateContainers`.
fn validate_containers(containers: &[Value], volumes: &BTreeSet<String>, path: &Path) -> ErrorList {
    if containers.is_empty() {
        return vec![required(path, "")];
    }
    let mut errs = Vec::new();
    let mut names = BTreeSet::new();
    for (i, c) in containers.iter().enumerate() {
        let p = path.index(i);
        errs.extend(container_common(c, volumes, &p));
        let name = str_of(c, "name");
        if !names.insert(name.to_string()) {
            errs.push(duplicate(&p.child("name"), Bad::str(name)));
        }
        if let Some(lifecycle) = c.get("lifecycle").filter(|l| !l.is_null()) {
            errs.extend(validate_lifecycle(lifecycle, &p.child("lifecycle")));
        }
        errs.extend(liveness_or_startup(&c["livenessProbe"], &p.child("livenessProbe")));
        errs.extend(readiness(&c["readinessProbe"], &p.child("readinessProbe")));
        errs.extend(liveness_or_startup(&c["startupProbe"], &p.child("startupProbe")));
        if has(c, "restartPolicy") {
            errs.push(forbidden(
                &p.child("restartPolicy"),
                "may not be set for non-init containers",
            ));
        }
    }
    errs.extend(host_port_conflicts(containers, path));
    errs
}

/// `validateInitContainers`.
fn validate_init_containers(
    containers: &[Value],
    regular: &[Value],
    volumes: &BTreeSet<String>,
    path: &Path,
) -> ErrorList {
    let mut errs = Vec::new();
    let mut names: BTreeSet<String> = regular.iter().map(|c| str_of(c, "name").to_string()).collect();
    for (i, c) in containers.iter().enumerate() {
        let p = path.index(i);
        errs.extend(container_common(c, volumes, &p));
        let restart_always = str_of(c, "restartPolicy") == "Always";
        if let Some(policy) = c.get("restartPolicy").and_then(Value::as_str) {
            if policy != "Always" {
                errs.push(not_supported(&p.child("restartPolicy"), Bad::str(policy), &["Always"]));
            }
        }
        let name = str_of(c, "name");
        if names.contains(name) {
            errs.push(duplicate(&p.child("name"), Bad::str(name)));
        } else if !name.is_empty() {
            names.insert(name.to_string());
        }
        errs.extend(host_port_conflicts(std::slice::from_ref(c), path));
        if restart_always {
            if let Some(lifecycle) = c.get("lifecycle").filter(|l| !l.is_null()) {
                errs.extend(validate_lifecycle(lifecycle, &p.child("lifecycle")));
            }
            errs.extend(liveness_or_startup(&c["livenessProbe"], &p.child("livenessProbe")));
            errs.extend(readiness(&c["readinessProbe"], &p.child("readinessProbe")));
            errs.extend(liveness_or_startup(&c["startupProbe"], &p.child("startupProbe")));
        } else {
            for key in ["lifecycle", "livenessProbe", "readinessProbe", "startupProbe"] {
                if has(c, key) {
                    errs.push(forbidden(
                        &p.child(key),
                        "may not be set for init containers without restartPolicy=Always",
                    ));
                }
            }
        }
    }
    errs
}

/// `validateContainerCommon`.
fn container_common(c: &Value, volumes: &BTreeSet<String>, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let name = str_of(c, "name");
    if name.is_empty() {
        errs.push(required(&path.child("name"), ""));
    } else {
        errs.extend(dns1123_label(name, &path.child("name")));
    }
    if str_of(c, "image").is_empty() {
        errs.push(required(&path.child("image"), ""));
    }
    match str_of(c, "terminationMessagePolicy") {
        "File" | "FallbackToLogsOnError" => {}
        "" => errs.push(required(&path.child("terminationMessagePolicy"), "")),
        other => errs.push(not_supported(
            &path.child("terminationMessagePolicy"),
            Bad::str(other),
            &["File", "FallbackToLogsOnError"],
        )),
    }
    errs.extend(container_ports(arr(c, "ports"), &path.child("ports")));
    errs.extend(validate_env(arr(c, "env"), &path.child("env")));
    errs.extend(validate_env_from(arr(c, "envFrom"), &path.child("envFrom")));
    errs.extend(volume_mounts(
        arr(c, "volumeMounts"),
        volumes,
        &path.child("volumeMounts"),
    ));
    match str_of(c, "imagePullPolicy") {
        "Always" | "IfNotPresent" | "Never" => {}
        "" => errs.push(required(&path.child("imagePullPolicy"), "")),
        other => errs.push(not_supported(
            &path.child("imagePullPolicy"),
            Bad::str(other),
            &["Always", "IfNotPresent", "Never"],
        )),
    }
    errs.extend(container_resources(&c["resources"], &path.child("resources")));
    errs.extend(security_context(&c["securityContext"], &path.child("securityContext")));
    errs
}

fn container_ports(ports: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut names = BTreeSet::new();
    for (i, port) in ports.iter().enumerate() {
        let p = path.index(i);
        let name = str_of(port, "name");
        if !name.is_empty() {
            let msgs = is_valid_port_name(name);
            if !msgs.is_empty() {
                for m in msgs {
                    errs.push(invalid(&p.child("name"), Bad::str(name), &m));
                }
            } else if !names.insert(name.to_string()) {
                errs.push(duplicate(&p.child("name"), Bad::str(name)));
            }
        }
        let container_port = i64_of(port, "containerPort");
        if container_port == 0 {
            errs.push(required(&p.child("containerPort"), ""));
        } else {
            for m in is_valid_port_num(container_port) {
                errs.push(invalid(&p.child("containerPort"), Bad::int(container_port), &m));
            }
        }
        let host_port = i64_of(port, "hostPort");
        if host_port != 0 {
            for m in is_valid_port_num(host_port) {
                errs.push(invalid(&p.child("hostPort"), Bad::int(host_port), &m));
            }
        }
        let protocol = str_of(port, "protocol");
        if protocol.is_empty() {
            errs.push(required(&p.child("protocol"), ""));
        } else if !PROTOCOLS.contains(&protocol) {
            errs.push(not_supported(&p.child("protocol"), Bad::str(protocol), PROTOCOLS));
        }
    }
    errs
}

/// `AccumulateUniqueHostPorts`.
fn host_port_conflicts(containers: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut seen = BTreeSet::new();
    for (ci, c) in containers.iter().enumerate() {
        for (pi, port) in arr(c, "ports").iter().enumerate() {
            let host_port = i64_of(port, "hostPort");
            if host_port == 0 {
                continue;
            }
            let key = format!("{}/{}/{host_port}", str_of(port, "protocol"), str_of(port, "hostIP"));
            if !seen.insert(key.clone()) {
                errs.push(duplicate(
                    &path.index(ci).child("ports").index(pi).child("hostPort"),
                    Bad::Str(key),
                ));
            }
        }
    }
    errs
}

const ENV_FIELD_PATHS: &[&str] = &[
    "metadata.name",
    "metadata.namespace",
    "metadata.uid",
    "spec.nodeName",
    "spec.serviceAccountName",
    "status.hostIP",
    "status.hostIPs",
    "status.podIP",
    "status.podIPs",
];

const RESOURCE_FIELD_PATHS: &[&str] = &[
    "limits.cpu",
    "limits.ephemeral-storage",
    "limits.memory",
    "requests.cpu",
    "requests.ephemeral-storage",
    "requests.memory",
];

/// The field paths a pod's downward API accepts (`ConvertDownwardAPIFieldLabel`).
const CONVERTIBLE_FIELD_PATHS: &[&str] = &[
    "metadata.annotations",
    "metadata.labels",
    "metadata.name",
    "metadata.namespace",
    "metadata.uid",
    "spec.nodeName",
    "spec.restartPolicy",
    "spec.serviceAccountName",
    "spec.schedulerName",
    "status.phase",
    "status.hostIP",
    "status.hostIPs",
    "status.podIP",
    "status.podIPs",
];

fn validate_env(vars: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for (i, ev) in vars.iter().enumerate() {
        let p = path.index(i);
        let name = str_of(ev, "name");
        if name.is_empty() {
            errs.push(required(&p.child("name"), ""));
        } else {
            for m in is_relaxed_env_var_name(name) {
                errs.push(invalid(&p.child("name"), Bad::str(name), &m));
            }
        }
        errs.extend(env_value_from(ev, &p.child("valueFrom")));
    }
    errs
}

/// `validateEnvVarValueFrom`.
fn env_value_from(ev: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let Some(from) = ev.get("valueFrom").filter(|v| !v.is_null()) else {
        return errs;
    };
    let mut sources = 0;
    if let Some(fs) = from.get("fieldRef").filter(|v| !v.is_null()) {
        sources += 1;
        errs.extend(object_field_selector(fs, &path.child("fieldRef")));
    }
    if let Some(rs) = from.get("resourceFieldRef").filter(|v| !v.is_null()) {
        sources += 1;
        let p = path.child("resourceFieldRef");
        let resource = str_of(rs, "resource");
        if resource.is_empty() {
            errs.push(required(&p.child("resource"), ""));
        } else if !RESOURCE_FIELD_PATHS.contains(&resource)
            && !resource.starts_with("limits.hugepages-")
            && !resource.starts_with("requests.hugepages-")
        {
            errs.push(not_supported(
                &p.child("resource"),
                Bad::str(resource),
                RESOURCE_FIELD_PATHS,
            ));
        }
    }
    for (key, label) in [("configMapKeyRef", "configMapKeyRef"), ("secretKeyRef", "secretKeyRef")] {
        if let Some(sel) = from.get(key).filter(|v| !v.is_null()) {
            sources += 1;
            let p = path.child(label);
            errs.extend(name_rule(NameRule::Subdomain, str_of(sel, "name"), &p.child("name")));
            let k = str_of(sel, "key");
            if k.is_empty() {
                errs.push(required(&p.child("key"), ""));
            } else {
                for m in is_config_map_key(k) {
                    errs.push(invalid(&p.child("key"), Bad::str(k), &m));
                }
            }
        }
    }
    if let Some(sel) = from.get("fileKeyRef").filter(|v| !v.is_null()) {
        sources += 1;
        let k = str_of(sel, "key");
        if k.is_empty() {
            errs.push(required(&path.child("fileKeyRef").child("key"), ""));
        } else {
            for m in is_relaxed_env_var_name(k) {
                errs.push(invalid(&path.child("fileKeyRef").child("key"), Bad::str(k), &m));
            }
        }
    }
    if sources == 0 {
        errs.push(invalid(
            path,
            Bad::str(""),
            "must specify one of: `fieldRef`, `resourceFieldRef`, `configMapKeyRef`, `secretKeyRef` or `fileKeyRef`",
        ));
    } else if !str_of(ev, "value").is_empty() {
        errs.push(invalid(
            path,
            Bad::str(""),
            "may not be specified when `value` is not empty",
        ));
    } else if sources > 1 {
        errs.push(invalid(
            path,
            Bad::str(""),
            "may not have more than one field specified at a time",
        ));
    }
    errs
}

/// `validateObjectFieldSelector` with the env var field paths.
fn object_field_selector(fs: &Value, path: &Path) -> ErrorList {
    let api_version = str_of(fs, "apiVersion");
    if api_version.is_empty() {
        return vec![required(&path.child("apiVersion"), "")];
    }
    let field_path = str_of(fs, "fieldPath");
    if field_path.is_empty() {
        return vec![required(&path.child("fieldPath"), "")];
    }
    let (base, subscript) = match field_path.split_once('[') {
        Some((base, rest)) if rest.ends_with(']') => (base, Some(&rest[..rest.len() - 1])),
        _ => (field_path, None),
    };
    let convertible = CONVERTIBLE_FIELD_PATHS.contains(&base)
        && (subscript.is_none() || matches!(base, "metadata.annotations" | "metadata.labels"));
    if !convertible {
        return vec![invalid(
            &path.child("fieldPath"),
            Bad::str(field_path),
            &format!("error converting fieldPath: field label not supported: {field_path}"),
        )];
    }
    match (base, subscript) {
        ("metadata.annotations", Some(s)) => qualified_name(&s.trim_matches('\'').to_lowercase(), path),
        ("metadata.labels", Some(s)) => qualified_name(s.trim_matches('\''), path),
        (_, None) if !ENV_FIELD_PATHS.contains(&base) => {
            vec![not_supported(&path.child("fieldPath"), Bad::str(base), ENV_FIELD_PATHS)]
        }
        _ => Vec::new(),
    }
}

/// `ValidateEnvFrom`.
fn validate_env_from(vars: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for (i, ev) in vars.iter().enumerate() {
        let p = path.index(i);
        let prefix = str_of(ev, "prefix");
        if !prefix.is_empty() {
            for m in is_relaxed_env_var_name(prefix) {
                errs.push(invalid(&p.child("prefix"), Bad::str(prefix), &m));
            }
        }
        let mut sources = 0;
        for key in ["configMapRef", "secretRef"] {
            if let Some(r) = ev.get(key).filter(|v| !v.is_null()) {
                sources += 1;
                let name = str_of(r, "name");
                if name.is_empty() {
                    errs.push(required(&p.child(key).child("name"), ""));
                } else {
                    for m in NameRule::Subdomain.check(name, true) {
                        errs.push(invalid(&p.child(key).child("name"), Bad::str(name), &m));
                    }
                }
            }
        }
        if sources == 0 {
            errs.push(invalid(
                path,
                Bad::str(""),
                "must specify one of: `configMapRef` or `secretRef`",
            ));
        } else if sources > 1 {
            errs.push(invalid(
                path,
                Bad::str(""),
                "may not have more than one field specified at a time",
            ));
        }
    }
    errs
}

/// `ValidateVolumeMounts`.
fn volume_mounts(mounts: &[Value], volumes: &BTreeSet<String>, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut points = BTreeSet::new();
    for (i, m) in mounts.iter().enumerate() {
        let p = path.index(i);
        let name = str_of(m, "name");
        if name.is_empty() {
            errs.push(required(&p.child("name"), ""));
        }
        if !volumes.contains(name) {
            errs.push(not_found(&p.child("name"), Bad::str(name)));
        }
        let mount_path = str_of(m, "mountPath");
        if mount_path.is_empty() {
            errs.push(required(&p.child("mountPath"), ""));
        }
        if !points.insert(mount_path.to_string()) {
            errs.push(invalid(&p.child("mountPath"), Bad::str(mount_path), "must be unique"));
        }
        let sub_path = str_of(m, "subPath");
        if !sub_path.is_empty() {
            errs.extend(local_descending_path(sub_path, &path.child("subPath")));
        }
        let sub_path_expr = str_of(m, "subPathExpr");
        if !sub_path_expr.is_empty() {
            if !sub_path.is_empty() {
                errs.push(invalid(
                    &p.child("subPathExpr"),
                    Bad::str(sub_path_expr),
                    "subPathExpr and subPath are mutually exclusive",
                ));
            }
            errs.extend(local_descending_path(sub_path_expr, &path.child("subPathExpr")));
        }
        if let Some(mode) = m.get("mountPropagation").and_then(Value::as_str) {
            let supported = ["Bidirectional", "HostToContainer", "None"];
            if !supported.contains(&mode) {
                errs.push(not_supported(
                    &path.child("mountPropagation"),
                    Bad::str(mode),
                    &supported,
                ));
            }
        }
    }
    errs
}

/// `validateProbe` and the liveness/startup rule (`successThreshold` must be 1).
fn liveness_or_startup(probe: &Value, path: &Path) -> ErrorList {
    if probe.is_null() {
        return Vec::new();
    }
    let mut errs = validate_probe(probe, path);
    let threshold = i64_of(probe, "successThreshold");
    if threshold != 1 {
        errs.push(invalid(
            &path.child("successThreshold"),
            Bad::int(threshold),
            "must be 1",
        ));
    }
    errs
}

fn readiness(probe: &Value, path: &Path) -> ErrorList {
    if probe.is_null() {
        return Vec::new();
    }
    let mut errs = validate_probe(probe, path);
    if let Some(g) = opt_i64(probe, "terminationGracePeriodSeconds") {
        errs.push(invalid(
            &path.child("terminationGracePeriodSeconds"),
            Bad::int(g),
            "must not be set for readinessProbes",
        ));
    }
    errs
}

fn validate_probe(probe: &Value, path: &Path) -> ErrorList {
    let mut errs = validate_handler(probe, path, &["exec", "httpGet", "tcpSocket", "grpc"]);
    for key in [
        "initialDelaySeconds",
        "timeoutSeconds",
        "periodSeconds",
        "successThreshold",
        "failureThreshold",
    ] {
        errs.extend(nonnegative(i64_of(probe, key), &path.child(key)));
    }
    if let Some(g) = opt_i64(probe, "terminationGracePeriodSeconds") {
        if g <= 0 {
            errs.push(invalid(
                &path.child("terminationGracePeriodSeconds"),
                Bad::int(g),
                "must be greater than 0",
            ));
        }
    }
    errs
}

fn validate_lifecycle(lifecycle: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for hook in ["postStart", "preStop"] {
        if let Some(h) = lifecycle.get(hook).filter(|h| !h.is_null()) {
            errs.extend(validate_handler(
                h,
                &path.child(hook),
                &["exec", "httpGet", "tcpSocket", "sleep"],
            ));
        }
    }
    errs
}

/// `validateHandler`: exactly one handler type, each checked.
fn validate_handler(h: &Value, path: &Path, kinds: &[&str]) -> ErrorList {
    let mut errs = Vec::new();
    let mut count = 0;
    for &kind in ["exec", "httpGet", "tcpSocket", "grpc", "sleep"].iter() {
        if !kinds.contains(&kind) || !has(h, kind) {
            continue;
        }
        let p = path.child(kind);
        if count > 0 {
            errs.push(forbidden(&p, "may not specify more than 1 handler type"));
            continue;
        }
        count += 1;
        let a = &h[kind];
        match kind {
            "exec" => {
                if arr(a, "command").is_empty() {
                    errs.push(required(&p.child("command"), ""));
                }
            }
            "httpGet" => {
                if str_of(a, "path").is_empty() {
                    errs.push(required(&p.child("path"), ""));
                }
                errs.extend(port_num_or_name(&a["port"], &p.child("port")));
                let scheme = str_of(a, "scheme");
                if scheme != "HTTP" && scheme != "HTTPS" {
                    errs.push(not_supported(&p.child("scheme"), Bad::str(scheme), &["HTTP", "HTTPS"]));
                }
                for header in arr(a, "httpHeaders") {
                    let name = str_of(header, "name");
                    for m in is_http_header_name(name) {
                        errs.push(invalid(&p.child("httpHeaders"), Bad::str(name), &m));
                    }
                }
            }
            "tcpSocket" => errs.extend(port_num_or_name(&a["port"], &p.child("port"))),
            "grpc" => errs.extend(port_num_or_name(&json!(i64_of(a, "port")), &p.child("port"))),
            _ => {}
        }
    }
    if count == 0 {
        errs.push(required(path, "must specify a handler type"));
    }
    errs
}

/// `ValidatePortNumOrName`.
fn port_num_or_name(port: &Value, path: &Path) -> ErrorList {
    match port {
        Value::String(name) => is_valid_port_name(name)
            .into_iter()
            .map(|m| invalid(path, Bad::str(name), &m))
            .collect(),
        other => {
            let n = other.as_i64().unwrap_or_default();
            is_valid_port_num(n)
                .into_iter()
                .map(|m| invalid(path, Bad::int(n), &m))
                .collect()
        }
    }
}

fn is_standard_container_resource(name: &str) -> bool {
    matches!(name, "cpu" | "memory" | "ephemeral-storage") || name.starts_with("hugepages-")
}

const STANDARD_RESOURCES: &[&str] = &[
    "cpu",
    "memory",
    "ephemeral-storage",
    "requests.cpu",
    "requests.memory",
    "requests.ephemeral-storage",
    "limits.cpu",
    "limits.memory",
    "limits.ephemeral-storage",
    "pods",
    "quota",
    "services",
    "replicationcontrollers",
    "resourcequotas",
    "secrets",
    "persistentvolumeclaims",
    "configmaps",
    "requests.storage",
    "services.nodeports",
    "services.loadbalancers",
];

fn is_native_resource(name: &str) -> bool {
    !name.contains('/') || name.contains("kubernetes.io/")
}

fn is_overcommit_allowed(name: &str) -> bool {
    is_native_resource(name) && !name.starts_with("hugepages-")
}

/// `validateContainerResourceName`.
fn container_resource_name(name: &str, path: &Path) -> ErrorList {
    let mut errs: ErrorList = is_qualified_name(name)
        .into_iter()
        .map(|m| invalid(path, Bad::str(name), &m))
        .collect();
    if errs.is_empty() && !name.contains('/') && !STANDARD_RESOURCES.contains(&name) && !name.starts_with("hugepages-")
    {
        errs.push(invalid(
            path,
            Bad::str(name),
            "must be a standard resource type or fully qualified",
        ));
    }
    if !name.contains('/') {
        if !is_standard_container_resource(name) {
            errs.push(invalid(
                path,
                Bad::str(name),
                "must be a standard resource for containers",
            ));
        }
    } else if !is_native_resource(name) && !is_qualified_name(&format!("requests.{name}")).is_empty() {
        errs.push(invalid(
            path,
            Bad::str(name),
            "doesn't follow extended resource name standard",
        ));
    }
    errs
}

fn quantity_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `ValidateContainerResourceRequirements`.
fn container_resources(r: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let (lim_path, req_path) = (path.child("limits"), path.child("requests"));
    let limits = obj(r, "limits");
    for (name, q) in limits.into_iter().flatten() {
        let p = lim_path.key(name);
        errs.extend(container_resource_name(name, &p));
        errs.extend(nonnegative_quantity(q, &p));
    }
    for (name, q) in obj(r, "requests").into_iter().flatten() {
        let p = req_path.key(name);
        errs.extend(container_resource_name(name, &p));
        errs.extend(nonnegative_quantity(q, &p));
        let text = quantity_text(q);
        match limits.and_then(|l| l.get(name)) {
            Some(limit) => {
                let limit_text = quantity_text(limit);
                let order = match (Quantity::parse(&text), Quantity::parse(&limit_text)) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    _ => Ordering::Equal,
                };
                if order != Ordering::Equal && !is_overcommit_allowed(name) {
                    errs.push(invalid(
                        &req_path,
                        Bad::Str(text.clone()),
                        &format!("must be equal to {name} limit of {limit_text}"),
                    ));
                } else if order == Ordering::Greater {
                    errs.push(invalid(
                        &req_path,
                        Bad::Str(text.clone()),
                        &format!("must be less than or equal to {name} limit of {limit_text}"),
                    ));
                }
            }
            None if !is_overcommit_allowed(name) => {
                errs.push(required(
                    &lim_path,
                    "Limit must be set for non overcommitable resources",
                ));
            }
            None => {}
        }
    }
    errs
}

/// `ValidateNonnegativeQuantity` (and the value printed as upstream's `String()`).
fn nonnegative_quantity(q: &Value, path: &Path) -> ErrorList {
    let text = quantity_text(q);
    match Quantity::parse(&text) {
        Some(parsed) if parsed.sign() < 0 => vec![invalid(path, Bad::Str(parsed.canonical()), meta::NEGATIVE)],
        _ => Vec::new(),
    }
}

/// `ValidateSecurityContext` (user and group IDs, privilege escalation).
fn security_context(sc: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    if sc.is_null() {
        return errs;
    }
    for key in ["runAsUser", "runAsGroup"] {
        if let Some(id) = opt_i64(sc, key) {
            for m in is_valid_user_id(id) {
                errs.push(invalid(&path.child(key), Bad::int(id), &m));
            }
        }
    }
    errs
}

/// `ValidateTolerations`.
fn validate_tolerations(tolerations: &[Value], path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    for (i, t) in tolerations.iter().enumerate() {
        let p = path.index(i);
        let key = str_of(t, "key");
        let operator = str_of(t, "operator");
        let value = str_of(t, "value");
        let effect = str_of(t, "effect");
        if !key.is_empty() {
            errs.extend(meta::label_name(key, &p.child("key")));
        }
        if key.is_empty() && operator != "Exists" {
            errs.push(invalid(
                &p.child("operator"),
                Bad::str(operator),
                "operator must be Exists when `key` is empty, which means \"match all values and all keys\"",
            ));
        }
        if has(t, "tolerationSeconds") && effect != "NoExecute" {
            errs.push(invalid(
                &p.child("effect"),
                Bad::str(effect),
                "effect must be 'NoExecute' when `tolerationSeconds` is set",
            ));
        }
        match operator {
            "Equal" | "" => {
                let msgs = is_valid_label_value(value);
                if !msgs.is_empty() {
                    errs.push(invalid(&p.child("operator"), Bad::str(value), &msgs.join(";")));
                }
            }
            "Exists" => {
                if !value.is_empty() {
                    errs.push(invalid(
                        &p.child("operator"),
                        Bad::str(value),
                        "value must be empty when `operator` is 'Exists'",
                    ));
                }
            }
            _ => errs.push(not_supported(
                &p.child("operator"),
                Bad::str(operator),
                &["Equal", "Exists"],
            )),
        }
        if !effect.is_empty() {
            let supported = ["NoSchedule", "PreferNoSchedule", "NoExecute"];
            if !supported.contains(&effect) {
                errs.push(not_supported(&p.child("effect"), Bad::str(effect), &supported));
            }
        }
    }
    errs
}

// ---- services ----

pub fn validate_service_create(svc: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&svc["metadata"], true, NameRule::Label);
    errs.extend(validate_service(svc, None));
    errs
}

pub fn validate_service_update(svc: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&svc["metadata"], &old["metadata"]);
    let (spec, old_spec) = (&svc["spec"], &old["spec"]);
    if str_of(spec, "type") != "ExternalName" && str_of(old_spec, "type") != "ExternalName" {
        let (new_ips, old_ips) = (arr(spec, "clusterIPs"), arr(old_spec, "clusterIPs"));
        let headless = |ips: &[Value]| ips.first().and_then(Value::as_str) == Some("None");
        if !(headless(new_ips) && headless(old_ips)) && new_ips.len() == old_ips.len() {
            for (i, ip) in old_ips.iter().enumerate() {
                if new_ips[i] != *ip {
                    errs.push(invalid(
                        &Path::new("spec").child("clusterIPs").index(i),
                        Bad::json(&Value::Array(new_ips.to_vec())),
                        "may not change once set",
                    ));
                }
            }
        }
    }
    errs.extend(validate_service(svc, Some(old)));
    errs
}

fn is_headless(spec: &Value) -> bool {
    arr(spec, "clusterIPs").first().and_then(Value::as_str) == Some("None")
}

fn externally_accessible(spec: &Value) -> bool {
    let t = str_of(spec, "type");
    t == "LoadBalancer" || t == "NodePort" || (t == "ClusterIP" && !arr(spec, "externalIPs").is_empty())
}

/// `validateService`.
fn validate_service(svc: &Value, old: Option<&Value>) -> ErrorList {
    let spec = &svc["spec"];
    let path = Path::new("spec");
    let mut errs = Vec::new();
    let service_type = str_of(spec, "type");
    let ports = arr(spec, "ports");
    if ports.is_empty() && !is_headless(spec) && service_type != "ExternalName" {
        errs.push(required(&path.child("ports"), ""));
    }
    match service_type {
        "LoadBalancer" | "NodePort" if is_headless(spec) => {
            errs.push(invalid(
                &path.child("clusterIPs").index(0),
                Bad::str("None"),
                &format!("may not be set to 'None' for {service_type} services"),
            ));
        }
        "ExternalName" => {
            if !arr(spec, "clusterIPs").is_empty() {
                errs.push(forbidden(
                    &path.child("clusterIPs"),
                    "may not be set for ExternalName services",
                ));
            }
            if !arr(spec, "ipFamilies").is_empty() {
                errs.push(forbidden(
                    &path.child("ipFamilies"),
                    "may not be set for ExternalName services",
                ));
            }
            if has(spec, "ipFamilyPolicy") {
                errs.push(forbidden(
                    &path.child("ipFamilyPolicy"),
                    "may not be set for ExternalName services",
                ));
            }
            let cname = str_of(spec, "externalName").trim_end_matches('.');
            if cname.is_empty() {
                errs.push(required(&path.child("externalName"), ""));
            } else {
                errs.extend(dns1123_subdomain(cname, &path.child("externalName")));
            }
        }
        _ => {}
    }
    let mut names = BTreeSet::new();
    for (i, port) in ports.iter().enumerate() {
        errs.extend(service_port(
            port,
            ports.len() > 1,
            &mut names,
            &path.child("ports").index(i),
        ));
    }
    if has(spec, "selector") {
        errs.extend(meta::labels(obj(spec, "selector"), &path.child("selector")));
    }
    match str_of(spec, "sessionAffinity") {
        "" => errs.push(required(&path.child("sessionAffinity"), "")),
        "ClientIP" | "None" => {}
        other => errs.push(not_supported(
            &path.child("sessionAffinity"),
            Bad::str(other),
            &["ClientIP", "None"],
        )),
    }
    match str_of(spec, "sessionAffinity") {
        "ClientIP" => {
            let p = path.child("sessionAffinityConfig");
            match spec["sessionAffinityConfig"]["clientIP"]["timeoutSeconds"].as_i64() {
                None => errs.push(required(&p, "when session affinity type is ClientIP")),
                Some(t) if t <= 0 || t > 86400 => errs.push(invalid(
                    &p.child("clientIP").child("timeoutSeconds"),
                    Bad::int(t),
                    "must be greater than 0 and less than 86400",
                )),
                Some(_) => {}
            }
        }
        "None" if has(spec, "sessionAffinityConfig") => errs.push(forbidden(
            &path.child("sessionAffinityConfig"),
            "must not be set when session affinity is None",
        )),
        _ => {}
    }
    errs.extend(cluster_ips(spec, old));
    for (i, ip) in arr(spec, "externalIPs").iter().enumerate() {
        let ip = ip.as_str().unwrap_or_default();
        if ip.parse::<std::net::IpAddr>().is_err() {
            errs.push(invalid(
                &path.child("externalIPs").index(i),
                Bad::str(ip),
                "must be a valid IP address, (e.g. 10.9.8.7 or 2001:db8::ffff)",
            ));
        }
    }
    let supported_types = ["ClusterIP", "ExternalName", "LoadBalancer", "NodePort"];
    if service_type.is_empty() {
        errs.push(required(&path.child("type"), ""));
    } else if !supported_types.contains(&service_type) {
        errs.push(not_supported(
            &path.child("type"),
            Bad::str(service_type),
            &supported_types,
        ));
    }
    if service_type == "ClusterIP" {
        for (i, port) in ports.iter().enumerate() {
            if i64_of(port, "nodePort") != 0 {
                errs.push(forbidden(
                    &path.child("ports").index(i).child("nodePort"),
                    "may not be used when `type` is 'ClusterIP'",
                ));
            }
        }
    }
    let mut node_ports = BTreeSet::new();
    for (i, port) in ports.iter().enumerate() {
        let node_port = i64_of(port, "nodePort");
        if node_port != 0 && !node_ports.insert((str_of(port, "protocol").to_string(), node_port)) {
            errs.push(duplicate(
                &path.child("ports").index(i).child("nodePort"),
                Bad::int(node_port),
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for (i, port) in ports.iter().enumerate() {
        let protocol = str_of(port, "protocol");
        let number = i64_of(port, "port");
        if !seen.insert((protocol.to_string(), number)) {
            errs.push(duplicate(
                &path.child("ports").index(i),
                Bad::Json(format!(
                    "{{\"Name\":\"\",\"Protocol\":{},\"AppProtocol\":null,\"Port\":{number},\"TargetPort\":0,\"NodePort\":0}}",
                    super::field::go_json(&json!(protocol))
                )),
            ));
        }
    }
    if has(spec, "allocateLoadBalancerNodePorts") && service_type != "LoadBalancer" {
        errs.push(forbidden(
            &path.child("allocateLoadBalancerNodePorts"),
            "may only be used when `type` is 'LoadBalancer'",
        ));
    }
    if service_type == "LoadBalancer" && !has(spec, "allocateLoadBalancerNodePorts") {
        errs.push(required(&Path::new("allocateLoadBalancerNodePorts"), ""));
    }
    let etp = str_of(spec, "externalTrafficPolicy");
    if !externally_accessible(spec) {
        if !etp.is_empty() {
            errs.push(invalid(
                &path.child("externalTrafficPolicy"),
                Bad::str(etp),
                "may only be set for externally-accessible services",
            ));
        }
    } else if etp.is_empty() {
        errs.push(required(&path.child("externalTrafficPolicy"), ""));
    } else if etp != "Cluster" && etp != "Local" {
        errs.push(not_supported(
            &path.child("externalTrafficPolicy"),
            Bad::str(etp),
            &["Cluster", "Local"],
        ));
    }
    match spec.get("internalTrafficPolicy").and_then(Value::as_str) {
        None if matches!(service_type, "NodePort" | "LoadBalancer" | "ClusterIP") => {
            errs.push(required(&path.child("internalTrafficPolicy"), ""));
        }
        Some(p) if p != "Cluster" && p != "Local" => {
            errs.push(not_supported(
                &path.child("internalTrafficPolicy"),
                Bad::str(p),
                &["Cluster", "Local"],
            ));
        }
        _ => {}
    }
    errs
}

/// `validateServicePort`.
fn service_port(port: &Value, require_name: bool, names: &mut BTreeSet<String>, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let name = str_of(port, "name");
    if require_name && name.is_empty() {
        errs.push(required(&path.child("name"), ""));
    } else if !name.is_empty() {
        errs.extend(dns1123_label(name, &path.child("name")));
        if !names.insert(name.to_string()) {
            errs.push(duplicate(&path.child("name"), Bad::str(name)));
        }
    }
    let number = i64_of(port, "port");
    for m in is_valid_port_num(number) {
        errs.push(invalid(&path.child("port"), Bad::int(number), &m));
    }
    let protocol = str_of(port, "protocol");
    if protocol.is_empty() {
        errs.push(required(&path.child("protocol"), ""));
    } else if !PROTOCOLS.contains(&protocol) {
        errs.push(not_supported(&path.child("protocol"), Bad::str(protocol), PROTOCOLS));
    }
    errs.extend(port_num_or_name(&port["targetPort"], &path.child("targetPort")));
    if let Some(app) = port.get("appProtocol").and_then(Value::as_str) {
        errs.extend(qualified_name(app, &path.child("appProtocol")));
    }
    errs
}

/// `ValidateServiceClusterIPsRelatedFields` (IPv4/IPv6 validity and the clusterIP pairing).
fn cluster_ips(spec: &Value, old: Option<&Value>) -> ErrorList {
    let mut errs = Vec::new();
    if str_of(spec, "type") == "ExternalName" {
        return errs;
    }
    let path = Path::new("spec").child("clusterIPs");
    let cluster_ip = str_of(spec, "clusterIP");
    let ips = arr(spec, "clusterIPs");
    if !cluster_ip.is_empty() {
        if ips.is_empty() {
            errs.push(required(&path, ""));
        } else if ips[0].as_str() != Some(cluster_ip) {
            errs.push(invalid(
                &path,
                Bad::json(&Value::Array(ips.to_vec())),
                "first value must match `clusterIP`",
            ));
        }
    } else if !ips.is_empty() {
        errs.push(invalid(
            &path,
            Bad::json(&Value::Array(ips.to_vec())),
            "must be empty when `clusterIP` is not specified",
        ));
    }
    let existing: Vec<&str> = old
        .map(|o| arr(&o["spec"], "clusterIPs").iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for (i, ip) in ips.iter().enumerate() {
        let ip = ip.as_str().unwrap_or_default();
        if i == 0 && ip == "None" {
            if ips.len() > 1 {
                errs.push(invalid(
                    &path,
                    Bad::json(&Value::Array(ips.to_vec())),
                    "'None' must be the first and only value",
                ));
            }
            continue;
        }
        if !existing.contains(&ip) && ip.parse::<std::net::IpAddr>().is_err() {
            errs.push(invalid(
                &path.index(i),
                Bad::str(ip),
                "must be a valid IP address, (e.g. 10.9.8.7 or 2001:db8::ffff)",
            ));
        }
    }
    if ips.len() > 2 {
        errs.push(invalid(
            &path,
            Bad::json(&Value::Array(ips.to_vec())),
            "may only hold up to 2 values",
        ));
    }
    errs
}

// ---- configmaps, secrets, namespaces ----

pub fn validate_config_map(cm: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&cm["metadata"], true, NameRule::Subdomain);
    let mut size = 0;
    let binary = obj(cm, "binaryData");
    for (key, value) in obj(cm, "data").into_iter().flatten() {
        let p = Path::new("data").key(key);
        for m in is_config_map_key(key) {
            errs.push(invalid(&p, Bad::str(key), &m));
        }
        if binary.is_some_and(|b| b.contains_key(key)) {
            errs.push(invalid(&p, Bad::str(key), "duplicate of key present in binaryData"));
        }
        size += value.as_str().map_or(0, str::len);
    }
    for (key, value) in binary.into_iter().flatten() {
        for m in is_config_map_key(key) {
            errs.push(invalid(&Path::new("binaryData").key(key), Bad::str(key), &m));
        }
        size += decoded_len(value);
    }
    if size > MAX_SECRET_SIZE {
        errs.push(too_long(&Path::new(""), MAX_SECRET_SIZE));
    }
    errs
}

pub fn validate_config_map_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    if bool_of(old, "immutable") {
        if !bool_of(new, "immutable") {
            errs.push(forbidden(
                &Path::new("immutable"),
                "field is immutable when `immutable` is set",
            ));
        }
        for key in ["data", "binaryData"] {
            if new.get(key) != old.get(key) {
                errs.push(forbidden(&Path::new(key), "field is immutable when `immutable` is set"));
            }
        }
    }
    errs.extend(validate_config_map(new));
    errs
}

fn decoded_len(v: &Value) -> usize {
    v.as_str().map_or(0, |s| s.trim_end_matches('=').len() * 3 / 4)
}

pub fn validate_secret(secret: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&secret["metadata"], true, NameRule::Subdomain);
    let data_path = Path::new("data");
    let data = obj(secret, "data");
    let mut size = 0;
    for (key, value) in data.into_iter().flatten() {
        for m in is_config_map_key(key) {
            errs.push(invalid(&data_path.key(key), Bad::str(key), &m));
        }
        size += decoded_len(value);
    }
    if size > MAX_SECRET_SIZE {
        errs.push(too_long(&data_path, MAX_SECRET_SIZE));
    }
    let has_key = |k: &str| data.is_some_and(|d| d.contains_key(k));
    let decoded = |k: &str| -> Vec<u8> {
        data.and_then(|d| d.get(k))
            .and_then(Value::as_str)
            .and_then(crate::shell::base64_decode)
            .unwrap_or_default()
    };
    match str_of(secret, "type") {
        "kubernetes.io/service-account-token" => {
            let name = secret["metadata"]["annotations"]["kubernetes.io/service-account.name"].as_str();
            if name.is_none_or(str::is_empty) {
                errs.push(required(
                    &Path::new("metadata")
                        .child("annotations")
                        .key("kubernetes.io/service-account.name"),
                    "",
                ));
            }
        }
        "kubernetes.io/dockercfg" | "kubernetes.io/dockerconfigjson" => {
            let key = if str_of(secret, "type") == "kubernetes.io/dockercfg" {
                ".dockercfg"
            } else {
                ".dockerconfigjson"
            };
            if !has_key(key) {
                errs.push(required(&data_path.key(key), ""));
            } else if let Err(e) = go_json_unmarshal_object(&decoded(key)) {
                errs.push(invalid(&data_path.key(key), Bad::str("<secret contents redacted>"), &e));
            }
        }
        "kubernetes.io/basic-auth" => {
            if !has_key("username") && !has_key("password") {
                errs.push(required(&data_path.key("username"), ""));
                errs.push(required(&data_path.key("password"), ""));
            }
        }
        "kubernetes.io/ssh-auth" => {
            if decoded("ssh-privatekey").is_empty() {
                errs.push(required(&data_path.key("ssh-privatekey"), ""));
            }
        }
        "kubernetes.io/tls" => {
            if !has_key("tls.crt") {
                errs.push(required(&data_path.key("tls.crt"), ""));
            }
            if !has_key("tls.key") {
                errs.push(required(&data_path.key("tls.key"), ""));
            }
        }
        _ => {}
    }
    errs
}

pub fn validate_secret_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    let (nt, ot) = (str_of(new, "type"), str_of(old, "type"));
    if nt != ot {
        errs.push(invalid(&Path::new("type"), Bad::str(nt), IMMUTABLE));
    }
    if bool_of(old, "immutable") {
        if !bool_of(new, "immutable") {
            errs.push(forbidden(
                &Path::new("immutable"),
                "field is immutable when `immutable` is set",
            ));
        }
        if new.get("data") != old.get("data") {
            errs.push(forbidden(
                &Path::new("data"),
                "field is immutable when `immutable` is set",
            ));
        }
    }
    errs.extend(validate_secret(new));
    errs
}

pub fn validate_namespace(ns: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&ns["metadata"], false, NameRule::Label);
    for f in arr(&ns["spec"], "finalizers") {
        let f = f.as_str().unwrap_or_default();
        let p = Path::new("spec").child("finalizers");
        errs.extend(meta::finalizer_name(f, &p));
        if !f.contains('/') && f != "kubernetes" {
            errs.push(invalid(
                &p,
                Bad::str(f),
                "name is neither a standard finalizer name nor is it fully qualified",
            ));
        }
    }
    errs
}

/// The kinds the port checks only the metadata of.
pub fn validate_simple(kind: &str, object: &Value) -> ErrorList {
    let namespaced = kind != "Node";
    meta::object_meta(&object["metadata"], namespaced, NameRule::Subdomain)
}

/// `json.Unmarshal(data, &map[string]interface{}{})`'s error text, if any.
fn go_json_unmarshal_object(data: &[u8]) -> Result<(), String> {
    super::gojsonerr::check(data)?;
    let value: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let kind = match value {
        Value::Object(_) => return Ok(()),
        Value::Null => return Ok(()),
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
    };
    Err(format!(
        "json: cannot unmarshal {kind} into Go value of type map[string]interface {{}}"
    ))
}
