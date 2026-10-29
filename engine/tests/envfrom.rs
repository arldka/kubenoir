//! End-to-end: a container's environment comes from ConfigMaps, Secrets and the pod's own
//! fields, resolved when the container starts.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"envfrom","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"db:1":{"pullSeconds":1,"app":"db-client@0.2.0","config":{
        "name":"db","version":"1","databaseUrlEnv":"DATABASE_URL","portEnv":"PORT",
        "database":{"host":"postgres.default.svc.cluster.local","port":5432,"user":"payments","name":"payments","password":"s3cret"}}}}}"#;

const GOOD_URL: &str = "postgres://payments:s3cret@postgres.default.svc.cluster.local:5432/payments";

fn engine() -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn pod(env: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: api\nspec:\n  restartPolicy: Never\n  containers:\n  - name: api\n    image: db:1\n{env}"
    )
}

fn apply(e: &mut Engine, name: &str, yaml: &str) -> String {
    e.write_file(name, yaml);
    e.eval(&format!("kubectl apply -f {name}"))
}

fn logs(e: &mut Engine) -> String {
    e.eval("kubectl logs api")
}

/// Why the first container is waiting, as the pod's status says it.
fn waiting_message(e: &mut Engine) -> String {
    e.eval("kubectl get pod api -o jsonpath='{.status.containerStatuses[0].state.waiting.message}'")
}

fn status(e: &mut Engine) -> String {
    e.eval("kubectl get pod api --no-headers")
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn key_references_and_dollar_expansion_build_the_connection_url() {
    let mut e = engine();
    apply(&mut e, "cm.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db-config\ndata:\n  host: postgres.default.svc.cluster.local\n");
    apply(
        &mut e,
        "secret.yaml",
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db-auth\nstringData:\n  password: s3cret\n",
    );
    let out = apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: DB_HOST\n      valueFrom: {configMapKeyRef: {name: db-config, key: host}}\n    - name: DB_PASSWORD\n      valueFrom: {secretKeyRef: {name: db-auth, key: password}}\n    - name: DATABASE_URL\n      value: postgres://payments:$(DB_PASSWORD)@$(DB_HOST):5432/payments\n"),
    );
    assert_eq!(out, "pod/api created");
    e.advance(10);
    let log = logs(&mut e);
    assert!(log.contains(&format!("connected to {GOOD_URL}")), "{log}");
    assert_eq!(status(&mut e), "Running");
}

#[test]
fn stringdata_is_stored_as_base64_data() {
    let mut e = engine();
    apply(&mut e, "secret.yaml", "apiVersion: v1\nkind: Secret\nmetadata:\n  name: s\ndata:\n  keep: a2VlcA==\n  swapped: b2xk\nstringData:\n  swapped: new\n  fresh: pass\n");
    assert_eq!(e.eval("kubectl get secret s -o jsonpath='{.data.fresh}'"), "cGFzcw==");
    assert_eq!(
        e.eval("kubectl get secret s -o jsonpath='{.data.swapped}' | base64 -d"),
        "new"
    );
    assert_eq!(
        e.eval("kubectl get secret s -o jsonpath='{.data.keep}' | base64 -d"),
        "keep"
    );
    assert_eq!(e.eval("kubectl get secret s -o jsonpath='{.stringData}'"), "");
    assert_eq!(
        e.eval("kubectl get secret s"),
        "NAME   TYPE     DATA   AGE\ns      Opaque   3      0s"
    );
}

#[test]
fn a_trailing_newline_in_a_secret_reaches_the_container() {
    let mut e = engine();
    e.eval("echo s3cret > pw.txt");
    assert_eq!(
        e.eval("kubectl create secret generic db-auth --from-file=password=pw.txt"),
        "secret/db-auth created"
    );
    let manifest = pod("    env:\n    - name: DB_PASSWORD\n      valueFrom: {secretKeyRef: {name: db-auth, key: password}}\n    - name: DATABASE_URL\n      value: postgres://payments:$(DB_PASSWORD)@postgres.default.svc.cluster.local:5432/payments\n");
    apply(&mut e, "pod.yaml", &manifest);
    e.advance(10);
    let log = logs(&mut e);
    assert!(
        log.contains("ERROR") && !log.contains("connected to"),
        "the newline broke the URL: {log}"
    );

    // The fix: no newline in the value. The old pod keeps what it got; a new one reads the new value.
    e.eval("echo -n s3cret > pw.txt");
    e.eval("kubectl delete secret db-auth");
    e.eval("kubectl create secret generic db-auth --from-file=password=pw.txt");
    e.advance(5);
    assert!(
        logs(&mut e).contains("ERROR"),
        "a pod that already started is unchanged"
    );
    e.eval("kubectl delete pod api --force");
    apply(&mut e, "pod.yaml", &manifest);
    e.advance(10);
    assert!(logs(&mut e).contains(&format!("connected to {GOOD_URL}")));
}

#[test]
fn env_from_with_prefixes_precedence_and_optional_sources() {
    let mut e = engine();
    apply(&mut e, "cm.yaml", &format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: base\ndata:\n  DATABASE_URL: {GOOD_URL}\n  bad key: skipped\n  1abc: skipped too\n"));
    apply(&mut e, "pod.yaml", &pod("    envFrom:\n    - configMapRef: {name: base}\n    - configMapRef: {name: nope, optional: true}\n    - secretRef: {name: nada, optional: true}\n"));
    e.advance(10);
    assert!(logs(&mut e).contains("connected to"));
    let events = e.eval("kubectl get events");
    assert!(
        events.contains("Keys [1abc, bad key] from the EnvFrom configMap default/base were skipped since they are considered invalid environment variable names."),
        "{events}"
    );

    // A prefix applies to every key; `env` beats `envFrom`.
    e.eval("kubectl delete pod api --force");
    apply(&mut e, "pod.yaml", &pod(&format!("    envFrom:\n    - prefix: APP_\n      configMapRef: {{name: base}}\n    env:\n    - name: DATABASE_URL\n      value: {GOOD_URL}\n")));
    e.advance(10);
    assert!(logs(&mut e).contains("connected to"), "env wins");
    e.eval("kubectl delete pod api --force");
    apply(
        &mut e,
        "pod.yaml",
        &pod("    envFrom:\n    - prefix: APP_\n      configMapRef: {name: base}\n"),
    );
    e.advance(10);
    assert!(
        logs(&mut e).contains("DATABASE_URL"),
        "the prefixed name is not the one the app reads"
    );
    assert!(!logs(&mut e).contains("connected to"));

    // Later sources win over earlier ones.
    apply(&mut e, "second.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: second\ndata:\n  DATABASE_URL: postgres://nobody@elsewhere:1/x\n");
    e.eval("kubectl delete pod api --force");
    apply(
        &mut e,
        "pod.yaml",
        &pod("    envFrom:\n    - configMapRef: {name: base}\n    - configMapRef: {name: second}\n"),
    );
    e.advance(10);
    assert!(logs(&mut e).contains("elsewhere"), "{}", logs(&mut e));
}

#[test]
fn a_missing_config_map_or_key_keeps_the_container_from_starting_until_it_exists() {
    let mut e = engine();
    apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: DATABASE_URL\n      valueFrom: {configMapKeyRef: {name: db-config, key: url}}\n"),
    );
    e.advance(15);
    assert_eq!(status(&mut e), "CreateContainerConfigError");
    let describe = e.eval("kubectl describe pod api");
    assert!(
        describe.contains("Reason:       CreateContainerConfigError"),
        "{describe}"
    );
    assert_eq!(waiting_message(&mut e), "configmap \"db-config\" not found");
    let events = e.eval("kubectl get events");
    assert!(
        events.contains("Warning")
            && events.contains("Failed")
            && events.contains("Error: configmap \"db-config\" not found"),
        "{events}"
    );
    assert!(
        e.eval("kubectl logs api").starts_with("Error from server (BadRequest)")
            || e.eval("kubectl logs api").contains("waiting to start")
    );

    // The ConfigMap appears, without the key: still not startable, and the message says why.
    apply(
        &mut e,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db-config\ndata:\n  other: x\n",
    );
    e.advance(15);
    assert_eq!(status(&mut e), "CreateContainerConfigError");
    assert_eq!(
        waiting_message(&mut e),
        "couldn't find key url in ConfigMap default/db-config"
    );

    // With the key it starts by itself, no restart needed.
    apply(
        &mut e,
        "cm.yaml",
        &format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db-config\ndata:\n  url: {GOOD_URL}\n"),
    );
    e.advance(15);
    assert_eq!(status(&mut e), "Running");
    assert!(logs(&mut e).contains("connected to"));
    // The pod never counted a restart for it.
    assert_eq!(
        e.eval("kubectl get pod api --no-headers").split_whitespace().nth(3),
        Some("0")
    );
}

#[test]
fn a_missing_secret_is_named_as_a_secret_and_optional_references_are_skipped() {
    let mut e = engine();
    apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: DB_PASSWORD\n      valueFrom: {secretKeyRef: {name: vault, key: pw}}\n"),
    );
    e.advance(15);
    assert_eq!(status(&mut e), "CreateContainerConfigError");
    assert_eq!(waiting_message(&mut e), "secret \"vault\" not found");

    e.eval("kubectl delete pod api --force");
    apply(&mut e, "pod.yaml", &pod("    env:\n    - name: DATABASE_URL\n      valueFrom: {secretKeyRef: {name: vault, key: pw, optional: true}}\n"));
    e.advance(10);
    // The variable is simply unset, and the app says so.
    assert_eq!(status(&mut e), "Error");
    assert!(
        logs(&mut e).contains("required environment variable DATABASE_URL is not set"),
        "{}",
        logs(&mut e)
    );
}

#[test]
fn only_a_container_start_reads_the_environment() {
    let mut e = engine();
    apply(
        &mut e,
        "cm.yaml",
        &format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db-config\ndata:\n  url: {GOOD_URL}\n"),
    );
    e.eval("kubectl create deployment api --image=db:1");
    e.eval(r#"kubectl patch deployment api -p '{"spec":{"template":{"spec":{"containers":[{"name":"db","env":[{"name":"DATABASE_URL","valueFrom":{"configMapKeyRef":{"name":"db-config","key":"url"}}}]}]}}}}'"#);
    e.advance(30);
    let running = e.eval("kubectl get pods --no-headers");
    assert!(running.contains("Running"), "{running}");

    // The ConfigMap changes underneath it: the running container is unaffected.
    apply(&mut e, "cm.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db-config\ndata:\n  url: postgres://nobody@elsewhere:1/x\n");
    e.advance(120);
    let still = e.eval("kubectl get pods --no-headers");
    assert!(still.contains("Running") && still.contains(" 0 "), "{still}");
    assert!(e.eval("kubectl logs deploy/api").contains("connected to"));

    // A new pod starts with the new value.
    e.eval("kubectl rollout restart deployment/api");
    e.advance(60);
    // The new pod cannot reach that database, so the old one is kept: find the pod that is not Running.
    let pods = e.eval("kubectl get pods --no-headers");
    let new_pod = pods
        .lines()
        .find(|l| !l.contains(" Running "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("a new pod that is not running: {pods}"))
        .to_string();
    let log = e.eval(&format!("kubectl logs {new_pod}"));
    assert!(log.contains("elsewhere"), "{log}");
}

#[test]
fn pod_fields_are_available_to_the_container() {
    let mut e = engine();
    apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: NS\n      valueFrom: {fieldRef: {fieldPath: metadata.namespace}}\n    - name: DATABASE_URL\n      value: postgres://payments:s3cret@postgres.$(NS).svc.cluster.local:5432/payments\n"),
    );
    e.advance(10);
    assert!(
        logs(&mut e).contains(&format!("connected to {GOOD_URL}")),
        "{}",
        logs(&mut e)
    );

    e.eval("kubectl delete pod api --force");
    apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: X\n      valueFrom: {fieldRef: {fieldPath: spec.nonsense}}\n"),
    );
    e.advance(15);
    assert_eq!(status(&mut e), "CreateContainerConfigError");
    assert_eq!(waiting_message(&mut e), "unsupported fieldPath: spec.nonsense");

    e.eval("kubectl delete pod api --force");
    apply(
        &mut e,
        "pod.yaml",
        &pod("    env:\n    - name: X\n      valueFrom: {resourceFieldRef: {resource: limits.cpu}}\n"),
    );
    e.advance(15);
    assert_eq!(
        waiting_message(&mut e),
        "resourceFieldRef is not available in this terminal"
    );
}
