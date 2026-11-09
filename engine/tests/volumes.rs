//! End-to-end: volumes. Containers read ConfigMap and Secret files, share scratch space, and see
//! changes to what they mount only as a real kubelet would show them.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"volumes","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{
        "watch:1":{"pullSeconds":1,"app":"watcher@0.1.0","config":{"name":"profiler","version":"1.0","reload":true}},
        "watch-once:1":{"pullSeconds":1,"app":"watcher@0.1.0","config":{"name":"profiler","version":"1.0","reload":false}},
        "scribe:1":{"pullSeconds":1,"app":"scribe@0.1.0","config":{"name":"scribe","version":"1.0"}}}}"#;

fn engine() -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn apply(e: &mut Engine, yaml: &str) -> String {
    e.write_file("apply.yaml", yaml);
    e.eval("kubectl apply -f apply.yaml")
}

fn config_map(mode: &str) -> String {
    format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: profile\ndata:\n  mode: {mode}\n")
}

/// A one-container pod running `image` with `env`, `volume_mounts` and `volumes` (YAML flow
/// style) filled in.
fn pod(name: &str, image: &str, files: &str, mounts: &str, volumes: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {name}\nspec:\n  containers:\n  - name: {name}\n    image: {image}\n    env:\n    - {{name: WATCH_FILES, value: \"{files}\"}}\n    - {{name: WATCH_INTERVAL, value: \"10\"}}\n    volumeMounts: {mounts}\n  volumes: {volumes}\n"
    )
}

fn profile_pod(name: &str, image: &str) -> String {
    pod(
        name,
        image,
        "/etc/profile/mode",
        "[{name: cfg, mountPath: /etc/profile}]",
        "[{name: cfg, configMap: {name: profile}}]",
    )
}

fn logs(e: &mut Engine, pod: &str) -> String {
    e.eval(&format!("kubectl logs {pod}"))
}

fn status(e: &mut Engine, pod: &str) -> String {
    e.eval(&format!("kubectl get pod {pod} --no-headers"))
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_container_reads_the_files_of_a_config_map() {
    let mut e = engine();
    apply(&mut e, &config_map("fast"));
    assert_eq!(apply(&mut e, &profile_pod("api", "watch:1")), "pod/api created");
    e.advance(5);
    let log = logs(&mut e, "api");
    assert!(log.contains("loaded /etc/profile/mode: fast"), "{log}");
    assert_eq!(status(&mut e, "api"), "Running");
}

#[test]
fn a_changed_config_map_reaches_a_running_container_about_a_minute_later_if_it_reloads() {
    let mut e = engine();
    apply(&mut e, &config_map("fast"));
    apply(&mut e, &profile_pod("hot", "watch:1"));
    apply(&mut e, &profile_pod("cold", "watch-once:1"));
    e.advance(5);
    apply(&mut e, &config_map("slow"));

    e.advance(45);
    assert!(
        !logs(&mut e, "hot").contains("reloaded"),
        "not yet: {}",
        logs(&mut e, "hot")
    );
    e.advance(40);
    assert!(
        logs(&mut e, "hot").contains("reloaded /etc/profile/mode: slow"),
        "{}",
        logs(&mut e, "hot")
    );
    // An app without hot reload never notices, however long it runs.
    e.advance(300);
    let cold = logs(&mut e, "cold");
    assert!(
        cold.contains("loaded /etc/profile/mode: fast") && !cold.contains("slow"),
        "{cold}"
    );

    // A new container reads the current content from its first moment.
    e.eval("kubectl delete pod cold --force");
    apply(&mut e, &profile_pod("cold", "watch-once:1"));
    e.advance(5);
    assert!(logs(&mut e, "cold").contains("loaded /etc/profile/mode: slow"));
}

#[test]
fn a_sub_path_mount_never_refreshes() {
    let mut e = engine();
    apply(&mut e, &config_map("fast"));
    let file_mount = pod(
        "pinned",
        "watch:1",
        "/app/mode",
        "[{name: cfg, mountPath: /app/mode, subPath: mode}]",
        "[{name: cfg, configMap: {name: profile}}]",
    );
    apply(&mut e, &file_mount);
    apply(&mut e, &profile_pod("live", "watch:1"));
    e.advance(5);
    apply(&mut e, &config_map("slow"));
    e.advance(300);

    assert!(logs(&mut e, "live").contains("reloaded /etc/profile/mode: slow"));
    let pinned = logs(&mut e, "pinned");
    assert!(
        pinned.contains("loaded /app/mode: fast") && !pinned.contains("reloaded"),
        "{pinned}"
    );

    // Only a new container sees the new content.
    e.eval("kubectl delete pod pinned --force");
    apply(&mut e, &file_mount);
    e.advance(5);
    assert!(logs(&mut e, "pinned").contains("loaded /app/mode: slow"));
}

const SIDECAR: &str = "\
apiVersion: v1
kind: Pod
metadata:
  name: shared
spec:
  restartPolicy: Always
  containers:
  - name: scribe
    image: scribe:1
    env:
    - {name: SCRIBE_FILE, value: /data/app.log}
    - {name: SCRIBE_INTERVAL, value: \"10\"}
    - {name: SCRIBE_COUNT, value: \"2\"}
    volumeMounts: [{name: scratch, mountPath: /data}]
  - name: tail
    image: watch:1
    env:
    - {name: WATCH_FILES, value: /data/app.log}
    - {name: WATCH_INTERVAL, value: \"10\"}
    volumeMounts: [{name: scratch, mountPath: /data, readOnly: true}]
  volumes:
  - {name: scratch, emptyDir: {}}
";

#[test]
fn containers_of_a_pod_share_scratch_space_while_they_run() {
    let mut e = engine();
    let shared = SIDECAR.replace("    - {name: SCRIBE_COUNT, value: \"2\"}\n", "");
    assert_eq!(apply(&mut e, &shared), "pod/shared created");
    e.advance(45);
    let scribe = e.eval("kubectl logs shared -c scribe");
    assert!(
        scribe.contains("wrote entry 1 to /data/app.log") && scribe.contains("wrote entry 3"),
        "{scribe}"
    );
    let tail = e.eval("kubectl logs shared -c tail");
    assert!(
        tail.contains("reloaded /data/app.log: entry 1 | entry 2 | entry 3"),
        "{tail}"
    );
}

#[test]
fn scratch_space_survives_a_container_restart_but_not_the_pod() {
    let mut e = engine();
    assert_eq!(apply(&mut e, SIDECAR), "pod/shared created");
    // The first run writes two entries and exits; the kubelet restarts it, and it adds two more
    // to the same file, which the sidecar sees.
    e.advance(80);
    let restarts = e.eval("kubectl get pod shared -o jsonpath='{.status.containerStatuses[0].restartCount}'");
    assert!(restarts.parse::<i32>().unwrap() >= 1, "restarted: {restarts}");
    let tail = e.eval("kubectl logs shared -c tail");
    assert!(tail.contains("entry 1 | entry 2 | entry 1 | entry 2"), "{tail}");

    // A new pod has new scratch space: only its own entries.
    e.eval("kubectl delete pod shared --force");
    assert_eq!(apply(&mut e, SIDECAR), "pod/shared created");
    // Before the new scribe's own restart, the file holds only what this pod wrote.
    e.advance(15);
    let fresh = e.eval("kubectl logs shared -c tail");
    assert!(fresh.contains("loaded /data/app.log: entry 1"), "{fresh}");
    assert!(fresh.contains("reloaded /data/app.log: entry 1 | entry 2"), "{fresh}");
    assert!(!fresh.contains("entry 2 | entry 1"), "the old file is gone: {fresh}");
}

#[test]
fn read_only_and_unmounted_paths_cannot_be_written() {
    let mut e = engine();
    apply(&mut e, &config_map("fast"));
    let scribe_pod = |name: &str, file: &str, mounts: &str, volumes: &str| {
        format!(
            "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {name}\nspec:\n  containers:\n  - name: {name}\n    image: scribe:1\n    env:\n    - {{name: SCRIBE_FILE, value: {file}}}\n    volumeMounts: {mounts}\n  volumes: {volumes}\n"
        )
    };
    apply(
        &mut e,
        &scribe_pod(
            "into-config",
            "/etc/profile/app.log",
            "[{name: cfg, mountPath: /etc/profile}]",
            "[{name: cfg, configMap: {name: profile}}]",
        ),
    );
    apply(
        &mut e,
        &scribe_pod(
            "into-ro",
            "/data/app.log",
            "[{name: s, mountPath: /data, readOnly: true}]",
            "[{name: s, emptyDir: {}}]",
        ),
    );
    apply(&mut e, &scribe_pod("nowhere", "/tmp/app.log", "[]", "[]"));
    e.advance(5);
    assert!(logs(&mut e, "into-config").contains("ERROR cannot write /etc/profile/app.log: Read-only file system"));
    assert!(logs(&mut e, "into-ro").contains("ERROR cannot write /data/app.log: Read-only file system"));
    assert!(logs(&mut e, "nowhere").contains("ERROR cannot write /tmp/app.log: No such file or directory"));
}

#[test]
fn a_volume_that_cannot_be_mounted_keeps_the_pod_in_container_creating() {
    let mut e = engine();
    assert_eq!(apply(&mut e, &profile_pod("api", "watch:1")), "pod/api created");
    e.advance(15);
    assert_eq!(status(&mut e, "api"), "ContainerCreating");
    let events = e.eval("kubectl get events");
    assert!(
        events.contains("Warning")
            && events.contains("FailedMount")
            && events.contains("MountVolume.SetUp failed for volume \"cfg\" : configmap \"profile\" not found"),
        "{events}"
    );
    assert!(
        !events.contains("Pulling"),
        "nothing starts before the volumes are mounted: {events}"
    );
    assert_eq!(e.eval("kubectl get pod api -o jsonpath='{.status.phase}'"), "Pending");

    // The ConfigMap appears: the pod starts at the next attempt, with no new pod.
    apply(&mut e, &config_map("fast"));
    e.advance(15);
    assert_eq!(status(&mut e, "api"), "Running");
    assert!(logs(&mut e, "api").contains("loaded /etc/profile/mode: fast"));
}

#[test]
fn a_kind_of_volume_the_terminal_does_not_have_fails_by_name() {
    let mut e = engine();
    apply(
        &mut e,
        &pod(
            "api",
            "watch:1",
            "/data/x",
            "[{name: data, mountPath: /data}]",
            "[{name: data, persistentVolumeClaim: {claimName: pvc}}]",
        ),
    );
    e.advance(15);
    assert_eq!(status(&mut e, "api"), "ContainerCreating");
    assert!(e
        .eval("kubectl get events")
        .contains("MountVolume.SetUp failed for volume \"data\" : volume type persistentVolumeClaim is not available in this terminal"));
}

#[test]
fn secrets_are_files_decoded_exactly_and_items_choose_names() {
    let mut e = engine();
    apply(
        &mut e,
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: creds\nstringData:\n  password: s3cret\n  user: badge\n",
    );
    apply(
        &mut e,
        &pod(
            "api",
            "watch-once:1",
            "/etc/creds/password,/etc/creds/user,/etc/creds/other",
            "[{name: sec, mountPath: /etc/creds}]",
            "[{name: sec, secret: {secretName: creds, items: [{key: password, path: password}]}}]",
        ),
    );
    e.advance(5);
    let log = logs(&mut e, "api");
    assert!(log.contains("loaded /etc/creds/password: s3cret"), "{log}");
    assert!(
        log.contains("cannot read /etc/creds/user: No such file or directory"),
        "only the listed items: {log}"
    );

    apply(&mut e, &config_map("fast"));
    apply(
        &mut e,
        &pod(
            "renamed",
            "watch-once:1",
            "/etc/profile/conf/main.cfg,/etc/profile/mode",
            "[{name: cfg, mountPath: /etc/profile}]",
            "[{name: cfg, configMap: {name: profile, items: [{key: mode, path: conf/main.cfg}]}}]",
        ),
    );
    e.advance(5);
    let log = logs(&mut e, "renamed");
    assert!(log.contains("loaded /etc/profile/conf/main.cfg: fast"), "{log}");
    assert!(log.contains("cannot read /etc/profile/mode"), "{log}");

    // An item that is not in the object keeps the pod from starting; an optional object may be absent.
    apply(
        &mut e,
        &pod(
            "bad-item",
            "watch-once:1",
            "/x",
            "[{name: cfg, mountPath: /etc/profile}]",
            "[{name: cfg, configMap: {name: profile, items: [{key: nope, path: n}]}}]",
        ),
    );
    e.advance(15);
    assert_eq!(status(&mut e, "bad-item"), "ContainerCreating");
    assert!(e
        .eval("kubectl get events")
        .contains("configmap references non-existent config key: nope"));
    apply(
        &mut e,
        &pod(
            "optional",
            "watch-once:1",
            "/etc/maybe/x",
            "[{name: maybe, mountPath: /etc/maybe}]",
            "[{name: maybe, configMap: {name: not-there, optional: true}}]",
        ),
    );
    e.advance(5);
    assert_eq!(status(&mut e, "optional"), "Running");
    assert!(logs(&mut e, "optional").contains("cannot read /etc/maybe/x: No such file or directory"));
}

#[test]
fn a_mount_must_name_a_volume_and_say_where() {
    let mut e = engine();
    let out = apply(
        &mut e,
        &pod("api", "watch:1", "/x", "[{name: nope, mountPath: /data}]", "[]"),
    );
    assert!(
        out.contains("spec.containers[0].volumeMounts[0].name: Not found: \"nope\""),
        "{out}"
    );
    let out = apply(
        &mut e,
        &pod("api", "watch:1", "/x", "[{name: v}]", "[{name: v, emptyDir: {}}]"),
    );
    assert!(
        out.contains("spec.containers[0].volumeMounts[0].mountPath: Required value"),
        "{out}"
    );
    let out = apply(
        &mut e,
        &pod(
            "api",
            "watch:1",
            "/x",
            "[]",
            "[{name: v, emptyDir: {}}, {name: v, emptyDir: {}}]",
        ),
    );
    assert!(out.contains("spec.volumes[1].name: Duplicate value: \"v\""), "{out}");
    assert_eq!(e.eval("kubectl get pods"), "No resources found in default namespace.");
    // A volume with no source is defaulted to an emptyDir (`SetDefaults_Volume`).
    let out = apply(&mut e, &pod("api", "watch:1", "/x", "[]", "[{name: v}]"));
    assert_eq!(out, "pod/api created");
}

#[test]
fn describe_shows_what_is_mounted_and_what_the_volumes_are() {
    let mut e = engine();
    apply(&mut e, &config_map("fast"));
    apply(
        &mut e,
        &pod(
            "api",
            "watch:1",
            "/etc/profile/mode",
            "[{name: cfg, mountPath: /etc/profile, readOnly: true}, {name: cfg, mountPath: /app/mode, subPath: mode}, {name: scratch, mountPath: /data}]",
            "[{name: cfg, configMap: {name: profile}}, {name: scratch, emptyDir: {}}, {name: sec, secret: {secretName: creds, optional: true}}, {name: data, persistentVolumeClaim: {claimName: pvc}}]",
        ),
    );
    e.advance(5);
    let describe = e.eval("kubectl describe pod api");
    for expected in [
        "    Mounts:",
        "      /etc/profile from cfg (ro)",
        "      /app/mode from cfg (rw,path=\"mode\")",
        "      /data from scratch (rw)",
        "Volumes:",
        "  cfg:",
        "    Type:      ConfigMap (a volume populated by a ConfigMap)",
        "    Name:      profile",
        "    Optional:  false",
        "  scratch:",
        "    Type:       EmptyDir (a temporary directory that shares a pod's lifetime)",
        "    SizeLimit:  <unset>",
        "  sec:",
        "    Type:        Secret (a volume populated by a Secret)",
        "    SecretName:  creds",
        "    Optional:    true",
        "    Type:       PersistentVolumeClaim (a reference to a PersistentVolumeClaim in the same namespace)",
        "    ClaimName:  pvc",
    ] {
        assert!(
            describe.lines().any(|l| l == expected),
            "missing {expected:?} in:\n{describe}"
        );
    }
    let mounts = describe.find("    Mounts:").unwrap();
    assert!(describe.find("Environment:").unwrap() < mounts && mounts < describe.find("Conditions:").unwrap());
    assert!(describe.find("Conditions:").unwrap() < describe.find("Volumes:").unwrap());
    assert!(describe.find("Volumes:").unwrap() < describe.find("QoS Class:").unwrap());
}
