//! Defaulting, ported from upstream `k8s.io/kubernetes/pkg/apis/{core,apps,batch,networking,
//! discovery}/v1` (v1.36): the hand-written `SetDefaults_*` functions and the generated
//! `SetObjectDefaults_*` walks that call them (including the `+default=` values the generator
//! inlines), with v1.36's default feature gates (`ImageVolume` GA, `JobPodReplacementPolicy` GA,
//! `PodLevelResources` beta, on; of its pod-level defaulting only the limits → requests copy is
//! ported). The API server runs these on every object it decodes, before validation;
//! `tests/fidelity.rs` checks the port against upstream (SPEC V36).
//!
//! The port works on JSON. Go's zero values are kept in mind throughout: a string field upstream
//! tests against `""` is "unset" here when missing, `null` or empty; a pointer is unset when
//! missing or `null`; a Go struct that is not a pointer always exists (its fields default even
//! when the manifest never mentions it). Quantities are re-encoded the way upstream marshals
//! them (`crate::quantity`).

use serde_json::{json, Map, Value};

use crate::quantity::Quantity;

type Obj = Map<String, Value>;

/// `SetObjectDefaults_<Kind>` for the simulated kinds; other kinds are left alone.
pub fn set_object_defaults(kind: &str, object: &mut Value) {
    let Some(o) = object.as_object_mut() else { return };
    match kind {
        "Pod" => {
            set_defaults_pod(o);
            pod_spec_walk(child(o, "spec"));
            let Some(status) = object_field(o, "status") else {
                return;
            };
            for list in [
                "initContainerStatuses",
                "containerStatuses",
                "ephemeralContainerStatuses",
            ] {
                for s in items(status, list) {
                    resource_list(s, "allocatedResources");
                    if let Some(r) = object_field(s, "resources") {
                        resource_list(r, "limits");
                        resource_list(r, "requests");
                    }
                }
            }
            resource_list(status, "allocatedResources");
            if let Some(r) = object_field(status, "resources") {
                resource_list(r, "limits");
                resource_list(r, "requests");
            }
        }
        "Service" => set_defaults_service(o),
        "Secret" => set_default_str(o, "type", "Opaque"),
        "ConfigMap" => {
            // `SetDefaults_ConfigMap` makes `data` an empty map, which encodes to nothing.
        }
        "Endpoints" => {
            for subset in items(o, "subsets") {
                for port in items(subset, "ports") {
                    set_default_str(port, "protocol", "TCP");
                }
            }
        }
        "Namespace" => {
            let name = o
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !name.is_empty() {
                let labels = child(child(o, "metadata"), "labels");
                labels.insert("kubernetes.io/metadata.name".into(), json!(name));
            }
            set_default_str(child(o, "status"), "phase", "Active");
        }
        "Node" => {
            let status = child(o, "status");
            if is_nil(status, "allocatable") && !is_nil(status, "capacity") {
                let capacity = status["capacity"].clone();
                status.insert("allocatable".into(), capacity);
            }
            resource_list(status, "capacity");
            resource_list(status, "allocatable");
        }
        "Deployment" => {
            set_defaults_deployment(o);
            pod_template_walk(child(o, "spec"));
        }
        "ReplicaSet" => {
            set_default(child(o, "spec"), "replicas", json!(1));
            pod_template_walk(child(o, "spec"));
        }
        "Job" => {
            set_defaults_job(o);
            job_spec_walk(child(o, "spec"));
        }
        "CronJob" => {
            let spec = child(o, "spec");
            set_default_str(spec, "concurrencyPolicy", "Allow");
            set_default(spec, "suspend", json!(false));
            set_default(spec, "successfulJobsHistoryLimit", json!(3));
            set_default(spec, "failedJobsHistoryLimit", json!(1));
            // Not `SetDefaults_Job`: only the walk below the job template.
            job_spec_walk(child(child(spec, "jobTemplate"), "spec"));
        }
        "NetworkPolicy" => {
            let spec = child(o, "spec");
            if is_empty(spec, "policyTypes") {
                let mut types = vec![json!("Ingress")];
                if !is_empty(spec, "egress") {
                    types.push(json!("Egress"));
                }
                spec.insert("policyTypes".into(), Value::Array(types));
            }
            for direction in ["ingress", "egress"] {
                for rule in items(spec, direction) {
                    for port in items(rule, "ports") {
                        set_default(port, "protocol", json!("TCP"));
                    }
                }
            }
        }
        "EndpointSlice" => {
            for port in items(o, "ports") {
                set_default(port, "name", json!(""));
                set_default(port, "protocol", json!("TCP"));
            }
        }
        _ => {}
    }
}

// ---- JSON with Go zero values ----

/// A non-pointer struct field: it always exists, so make it an object.
fn child<'a>(o: &'a mut Obj, key: &str) -> &'a mut Obj {
    let v = o.entry(key).or_insert_with(|| json!({}));
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().expect("just made an object")
}

/// A pointer-to-struct field, if set.
fn object_field<'a>(o: &'a mut Obj, key: &str) -> Option<&'a mut Obj> {
    o.get_mut(key).and_then(Value::as_object_mut)
}

/// The objects of a list field.
fn items<'a>(o: &'a mut Obj, key: &str) -> impl Iterator<Item = &'a mut Obj> {
    o.get_mut(key)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
}

/// A pointer (or map, slice) field that is unset.
fn is_nil(o: &Obj, key: &str) -> bool {
    o.get(key).is_none_or(Value::is_null)
}

/// `len(x) == 0` for a list or map field.
fn is_empty(o: &Obj, key: &str) -> bool {
    match o.get(key) {
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(m)) => m.is_empty(),
        _ => true,
    }
}

/// A string field upstream compares with `""`.
fn str_of<'a>(o: &'a Obj, key: &str) -> &'a str {
    o.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A number field upstream compares with `0`.
fn is_zero_number(o: &Obj, key: &str) -> bool {
    o.get(key).and_then(Value::as_f64).is_none_or(|n| n == 0.0)
}

/// `if obj.X == nil { obj.X = value }`.
fn set_default(o: &mut Obj, key: &str, value: Value) {
    if is_nil(o, key) {
        o.insert(key.into(), value);
    }
}

/// `if obj.X == "" { obj.X = value }`.
fn set_default_str(o: &mut Obj, key: &str, value: &str) {
    if str_of(o, key).is_empty() {
        o.insert(key.into(), json!(value));
    }
}

/// `if obj.X == 0 { obj.X = value }`.
fn set_default_number(o: &mut Obj, key: &str, value: i64) {
    if is_zero_number(o, key) {
        o.insert(key.into(), json!(value));
    }
}

/// `SetDefaults_ResourceList`: every quantity rounded up to millis and re-encoded.
fn resource_list(o: &mut Obj, key: &str) {
    let Some(list) = o.get_mut(key).and_then(Value::as_object_mut) else {
        return;
    };
    for value in list.values_mut() {
        // A number in the manifest (`cpu: 1`) is a quantity of that text.
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => continue,
        };
        if let Some(mut q) = Quantity::parse(&text) {
            q.round_up(-3);
            *value = json!(q.canonical());
        }
    }
}

/// A quantity outside a resource list (`emptyDir.sizeLimit`): only re-encoded.
fn quantity(o: &mut Obj, key: &str) {
    if let Some(value) = o.get_mut(key) {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return,
        };
        if let Some(q) = Quantity::parse(&text) {
            *value = json!(q.canonical());
        }
    }
}

// ---- core/v1 ----

/// `parsers.ParseImageName`'s tag: the tag, `latest` when the reference has neither tag nor
/// digest, empty when it has only a digest or does not parse.
fn image_tag(image: &str) -> String {
    let Some((name, tag, digest)) = split_reference(image) else {
        return String::new();
    };
    if !valid_name(name) {
        return String::new();
    }
    match (tag, digest) {
        (Some(tag), _) => tag.to_string(),
        (None, Some(_)) => String::new(),
        (None, None) => "latest".into(),
    }
}

/// `name[:tag][@digest]`, with the tag and digest checked against the reference grammar.
fn split_reference(image: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let (rest, digest) = match image.split_once('@') {
        Some((rest, digest)) => (rest, Some(digest)),
        None => (image, None),
    };
    if let Some(d) = digest {
        let (algorithm, hex) = d.split_once(':')?;
        let algo_ok = !algorithm.is_empty()
            && algorithm.starts_with(|c: char| c.is_ascii_alphabetic())
            && algorithm
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+.".contains(c));
        if !algo_ok || hex.len() < 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
    }
    let last_slash = rest.rfind('/').map_or(0, |i| i + 1);
    let (name, tag) = match rest[last_slash..].rfind(':') {
        Some(i) => (&rest[..last_slash + i], Some(&rest[last_slash + i + 1..])),
        None => (rest, None),
    };
    if let Some(t) = tag {
        let ok = !t.is_empty()
            && t.len() <= 128
            && t.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
        if !ok {
            return None;
        }
    }
    Some((name, tag, digest))
}

/// The repository name grammar: an optional domain, then lowercase path components.
fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    let mut parts: Vec<&str> = name.split('/').collect();
    let first = parts[0];
    let has_domain = parts.len() > 1
        && (first.contains('.')
            || first.contains(':')
            || first == "localhost"
            || first.chars().any(|c| c.is_ascii_uppercase()));
    if has_domain {
        let (host, port) = match first.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (first, None),
        };
        let host_ok = host.split('.').all(|c| {
            !c.is_empty()
                && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                && !c.starts_with('-')
                && !c.ends_with('-')
        });
        if !host_ok || port.is_some_and(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
            return false;
        }
        parts.remove(0);
    }
    // A bare 64-hex-digit name is an image ID, not a reference.
    if parts.len() == 1
        && parts[0].len() == 64
        && parts[0]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return false;
    }
    parts.iter().all(|p| path_component_ok(p))
}

/// `[a-z0-9]+(?:(?:[._]|__|[-]+)[a-z0-9]+)*`.
fn path_component_ok(p: &str) -> bool {
    let b = p.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    if b.is_empty() || !alnum(b[0]) || !alnum(b[b.len() - 1]) {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if alnum(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !alnum(b[i]) {
            i += 1;
        }
        let sep = &p[start..i];
        let ok = sep == "." || sep == "_" || sep == "__" || sep.bytes().all(|c| c == b'-');
        if !ok {
            return false;
        }
    }
    true
}

fn pull_policy_for(image: &str) -> &'static str {
    if image_tag(image) == "latest" {
        "Always"
    } else {
        "IfNotPresent"
    }
}

/// `SetDefaults_Pod` (Pods only; templates skip it).
fn set_defaults_pod(o: &mut Obj) {
    let spec = child(o, "spec");
    for list in ["containers", "initContainers"] {
        for c in items(spec, list) {
            let Some(resources) = object_field(c, "resources") else {
                continue;
            };
            let Some(limits) = resources.get("limits").and_then(Value::as_object).cloned() else {
                continue;
            };
            // An empty map encodes to nothing, so only a non-empty one is worth making.
            let mut requests = resources
                .get("requests")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in limits {
                requests.entry(key).or_insert(value);
            }
            if !requests.is_empty() {
                resources.insert("requests".into(), Value::Object(requests));
            }
        }
    }
    default_pod_requests(spec);
    set_default(spec, "enableServiceLinks", json!(true));
    if spec.get("hostNetwork").and_then(Value::as_bool) == Some(true) {
        for list in ["containers", "initContainers"] {
            for c in items(spec, list) {
                for port in items(c, "ports") {
                    if is_zero_number(port, "hostPort") {
                        let container_port = port.get("containerPort").cloned().unwrap_or(json!(0));
                        port.insert("hostPort".into(), container_port);
                    }
                }
            }
        }
    }
}

/// `defaultPodRequests`, the part that copies pod-level limits without a pod-level request
/// into the requests. (Not ported: summing the containers' requests into the pod's, and
/// `defaultHugePagePodLimits`; both only touch pod-level `spec.resources`.)
fn default_pod_requests(spec: &mut Obj) {
    let Some(resources) = object_field(spec, "resources") else {
        return;
    };
    let Some(limits) = resources.get("limits").and_then(Value::as_object).cloned() else {
        return;
    };
    if limits.is_empty() {
        return;
    }
    let mut requests = resources
        .get("requests")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in limits {
        let supported = key == "cpu" || key == "memory" || key.starts_with("hugepages-");
        if supported && !requests.contains_key(&key) {
            requests.insert(key, value);
        }
    }
    if !requests.is_empty() {
        resources.insert("requests".into(), Value::Object(requests));
    }
}

/// The part of the generated walk every pod spec gets (Pods and templates alike).
fn pod_spec_walk(spec: &mut Obj) {
    // SetDefaults_PodSpec
    set_default_str(spec, "dnsPolicy", "ClusterFirst");
    set_default_str(spec, "restartPolicy", "Always");
    set_default(spec, "securityContext", json!({}));
    set_default(spec, "terminationGracePeriodSeconds", json!(30));
    set_default_str(spec, "schedulerName", "default-scheduler");

    for volume in items(spec, "volumes") {
        volume_defaults(volume);
    }
    for list in ["initContainers", "containers", "ephemeralContainers"] {
        for c in items(spec, list) {
            container_defaults(c);
        }
    }
    resource_list(spec, "overhead");
    if let Some(resources) = object_field(spec, "resources") {
        resource_list(resources, "limits");
        resource_list(resources, "requests");
    }
}

/// The walk below a `PodTemplateSpec` field (`spec.template`).
fn pod_template_walk(parent: &mut Obj) {
    pod_spec_walk(child(child(parent, "template"), "spec"));
}

const VOLUME_SOURCES: &[&str] = &[
    "hostPath",
    "emptyDir",
    "gcePersistentDisk",
    "awsElasticBlockStore",
    "gitRepo",
    "secret",
    "nfs",
    "iscsi",
    "glusterfs",
    "persistentVolumeClaim",
    "rbd",
    "flexVolume",
    "cinder",
    "cephfs",
    "flocker",
    "downwardAPI",
    "fc",
    "azureFile",
    "configMap",
    "vsphereVolume",
    "quobyte",
    "azureDisk",
    "photonPersistentDisk",
    "projected",
    "portworxVolume",
    "scaleIO",
    "storageos",
    "csi",
    "ephemeral",
    "image",
];

/// `SetDefaults_Volume` and the generated walk of a volume's source.
fn volume_defaults(v: &mut Obj) {
    if VOLUME_SOURCES.iter().all(|s| is_nil(v, s)) {
        v.insert("emptyDir".into(), json!({}));
    }
    if let Some(image) = object_field(v, "image") {
        if str_of(image, "pullPolicy").is_empty() {
            let policy = pull_policy_for(str_of(image, "reference"));
            image.insert("pullPolicy".into(), json!(policy));
        }
    }
    if let Some(host_path) = object_field(v, "hostPath") {
        set_default(host_path, "type", json!(""));
    }
    if let Some(secret) = object_field(v, "secret") {
        set_default(secret, "defaultMode", json!(420));
    }
    if let Some(iscsi) = object_field(v, "iscsi") {
        set_default_str(iscsi, "iscsiInterface", "default");
    }
    if let Some(rbd) = object_field(v, "rbd") {
        set_default_str(rbd, "pool", "rbd");
        set_default_str(rbd, "user", "admin");
        set_default_str(rbd, "keyring", "/etc/ceph/keyring");
    }
    if let Some(downward) = object_field(v, "downwardAPI") {
        set_default(downward, "defaultMode", json!(420));
        for item in items(downward, "items") {
            if let Some(field_ref) = object_field(item, "fieldRef") {
                set_default_str(field_ref, "apiVersion", "v1");
            }
        }
    }
    if let Some(config_map) = object_field(v, "configMap") {
        set_default(config_map, "defaultMode", json!(420));
    }
    if let Some(disk) = object_field(v, "azureDisk") {
        set_default(disk, "cachingMode", json!("ReadWrite"));
        set_default(disk, "fsType", json!("ext4"));
        set_default(disk, "readOnly", json!(false));
        set_default(disk, "kind", json!("Shared"));
    }
    if let Some(projected) = object_field(v, "projected") {
        set_default(projected, "defaultMode", json!(420));
        for source in items(projected, "sources") {
            if let Some(downward) = object_field(source, "downwardAPI") {
                for item in items(downward, "items") {
                    if let Some(field_ref) = object_field(item, "fieldRef") {
                        set_default_str(field_ref, "apiVersion", "v1");
                    }
                }
            }
            if let Some(token) = object_field(source, "serviceAccountToken") {
                set_default(token, "expirationSeconds", json!(3600));
            }
        }
    }
    if let Some(scale_io) = object_field(v, "scaleIO") {
        set_default_str(scale_io, "storageMode", "ThinProvisioned");
        set_default_str(scale_io, "fsType", "xfs");
    }
    if let Some(empty_dir) = object_field(v, "emptyDir") {
        quantity(empty_dir, "sizeLimit");
    }
    if let Some(ephemeral) = object_field(v, "ephemeral") {
        if let Some(template) = object_field(ephemeral, "volumeClaimTemplate") {
            let spec = child(template, "spec");
            set_default(spec, "volumeMode", json!("Filesystem"));
            let resources = child(spec, "resources");
            resource_list(resources, "limits");
            resource_list(resources, "requests");
        }
    }
}

/// `SetDefaults_Container` and the generated walk of a container.
fn container_defaults(c: &mut Obj) {
    if str_of(c, "imagePullPolicy").is_empty() {
        let policy = pull_policy_for(str_of(c, "image"));
        c.insert("imagePullPolicy".into(), json!(policy));
    }
    set_default_str(c, "terminationMessagePath", "/dev/termination-log");
    set_default_str(c, "terminationMessagePolicy", "File");
    for port in items(c, "ports") {
        set_default_str(port, "protocol", "TCP");
    }
    for env in items(c, "env") {
        if let Some(value_from) = object_field(env, "valueFrom") {
            if let Some(field_ref) = object_field(value_from, "fieldRef") {
                set_default_str(field_ref, "apiVersion", "v1");
            }
            if let Some(file_key_ref) = object_field(value_from, "fileKeyRef") {
                set_default(file_key_ref, "optional", json!(false));
            }
        }
    }
    // `Resources` is a struct, not a pointer: it is always there (`resources: {}`).
    let resources = child(c, "resources");
    resource_list(resources, "limits");
    resource_list(resources, "requests");
    for probe in ["livenessProbe", "readinessProbe", "startupProbe"] {
        if let Some(p) = object_field(c, probe) {
            set_default_number(p, "timeoutSeconds", 1);
            set_default_number(p, "periodSeconds", 10);
            set_default_number(p, "successThreshold", 1);
            set_default_number(p, "failureThreshold", 3);
            if let Some(http_get) = object_field(p, "httpGet") {
                http_get_defaults(http_get);
            }
            if let Some(grpc) = object_field(p, "grpc") {
                set_default(grpc, "service", json!(""));
            }
        }
    }
    if let Some(lifecycle) = object_field(c, "lifecycle") {
        for hook in ["postStart", "preStop"] {
            if let Some(h) = object_field(lifecycle, hook) {
                if let Some(http_get) = object_field(h, "httpGet") {
                    http_get_defaults(http_get);
                }
            }
        }
    }
}

/// `SetDefaults_HTTPGetAction`.
fn http_get_defaults(h: &mut Obj) {
    set_default_str(h, "path", "/");
    set_default_str(h, "scheme", "HTTP");
}

/// `SetDefaults_Service` and the generated port walk.
fn set_defaults_service(o: &mut Obj) {
    let spec = child(o, "spec");
    set_default_str(spec, "sessionAffinity", "None");
    match str_of(spec, "sessionAffinity") {
        "None" => {
            spec.remove("sessionAffinityConfig");
        }
        "ClientIP" => {
            let has_timeout = spec
                .get("sessionAffinityConfig")
                .and_then(|c| c.get("clientIP"))
                .and_then(|c| c.get("timeoutSeconds"))
                .is_some_and(|t| !t.is_null());
            if !has_timeout {
                spec.insert(
                    "sessionAffinityConfig".into(),
                    json!({"clientIP": {"timeoutSeconds": 10800}}),
                );
            }
        }
        _ => {}
    }
    set_default_str(spec, "type", "ClusterIP");
    for port in items(spec, "ports") {
        set_default_str(port, "protocol", "TCP");
        let target_unset = match port.get("targetPort") {
            None | Some(Value::Null) => true,
            Some(Value::Number(n)) => n.as_f64() == Some(0.0),
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        };
        if target_unset {
            let number = port.get("port").cloned().unwrap_or(json!(0));
            port.insert("targetPort".into(), number);
        }
    }
    let service_type = str_of(spec, "type").to_string();
    let externally_accessible = service_type == "LoadBalancer"
        || service_type == "NodePort"
        || (service_type == "ClusterIP" && !is_empty(spec, "externalIPs"));
    if externally_accessible {
        set_default_str(spec, "externalTrafficPolicy", "Cluster");
    }
    if is_nil(spec, "internalTrafficPolicy")
        && matches!(service_type.as_str(), "NodePort" | "LoadBalancer" | "ClusterIP")
    {
        spec.insert("internalTrafficPolicy".into(), json!("Cluster"));
    }
    if service_type == "LoadBalancer" {
        set_default(spec, "allocateLoadBalancerNodePorts", json!(true));
        let ingresses = object_field(o, "status")
            .and_then(|s| object_field(s, "loadBalancer"))
            .and_then(|lb| lb.get_mut("ingress"))
            .and_then(Value::as_array_mut);
        for ingress in ingresses.into_iter().flatten().filter_map(Value::as_object_mut) {
            if !str_of(ingress, "ip").is_empty() && is_nil(ingress, "ipMode") {
                ingress.insert("ipMode".into(), json!("VIP"));
            }
        }
    }
}

// ---- apps/v1, batch/v1 ----

/// `SetDefaults_Deployment`.
fn set_defaults_deployment(o: &mut Obj) {
    let spec = child(o, "spec");
    set_default(spec, "replicas", json!(1));
    let strategy = child(spec, "strategy");
    set_default_str(strategy, "type", "RollingUpdate");
    if str_of(strategy, "type") == "RollingUpdate" {
        let rolling = match strategy.get("rollingUpdate") {
            Some(Value::Object(_)) => strategy
                .get_mut("rollingUpdate")
                .and_then(Value::as_object_mut)
                .expect("object"),
            _ => {
                strategy.insert("rollingUpdate".into(), json!({}));
                strategy
                    .get_mut("rollingUpdate")
                    .and_then(Value::as_object_mut)
                    .expect("object")
            }
        };
        set_default(rolling, "maxUnavailable", json!("25%"));
        set_default(rolling, "maxSurge", json!("25%"));
    }
    set_default(spec, "revisionHistoryLimit", json!(10));
    set_default(spec, "progressDeadlineSeconds", json!(600));
}

/// `SetDefaults_Job`.
fn set_defaults_job(o: &mut Obj) {
    let template_labels = o
        .get("spec")
        .and_then(|s| s.get("template"))
        .and_then(|t| t.get("metadata"))
        .and_then(|m| m.get("labels"))
        .filter(|l| l.is_object())
        .cloned();
    let spec = child(o, "spec");
    if is_nil(spec, "completions") && is_nil(spec, "parallelism") {
        spec.insert("completions".into(), json!(1));
        spec.insert("parallelism".into(), json!(1));
    }
    set_default(spec, "parallelism", json!(1));
    if is_nil(spec, "backoffLimit") {
        let limit = if is_nil(spec, "backoffLimitPerIndex") {
            6
        } else {
            i32::MAX as i64
        };
        spec.insert("backoffLimit".into(), json!(limit));
    }
    set_default(spec, "completionMode", json!("NonIndexed"));
    set_default(spec, "suspend", json!(false));
    if is_nil(spec, "podReplacementPolicy") {
        let policy = if is_nil(spec, "podFailurePolicy") {
            "TerminatingOrFailed"
        } else {
            "Failed"
        };
        spec.insert("podReplacementPolicy".into(), json!(policy));
    }
    set_default(spec, "manualSelector", json!(false));
    if let Some(labels) = template_labels {
        let meta = child(o, "metadata");
        if is_empty(meta, "labels") {
            meta.insert("labels".into(), labels);
        }
    }
}

/// The generated walk below a `JobSpec`: pod failure policy patterns, then the pod template.
fn job_spec_walk(spec: &mut Obj) {
    if let Some(policy) = object_field(spec, "podFailurePolicy") {
        for rule in items(policy, "rules") {
            for pattern in items(rule, "onPodConditions") {
                set_default_str(pattern, "status", "True");
            }
        }
    }
    pod_template_walk(spec);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_tags_like_parse_image_name() {
        for (image, tag) in [
            ("nginx", "latest"),
            ("nginx:1.27", "1.27"),
            ("nginx:latest", "latest"),
            ("registry.local:5000/team/app", "latest"),
            ("registry.local:5000/team/app:v2", "v2"),
            (
                "app@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "",
            ),
            (
                "app:v1@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "v1",
            ),
            ("Shop:1.0", ""),
            ("shop:", ""),
        ] {
            assert_eq!(image_tag(image), tag, "{image}");
        }
    }
}
