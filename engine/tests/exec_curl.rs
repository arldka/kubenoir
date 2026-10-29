//! End-to-end: `kubectl exec POD -- curl` follows DNS, Services, ready endpoints and the port the
//! target app really listens on (V9, V10).

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"curl","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"informant:1":{"pullSeconds":1,"app":"http-responder@0.1.0","config":{"name":"informant","version":"1.2","portEnv":"PORT"}},
              "toolbox:1":{"pullSeconds":1,"app":"nginx@0.1.0","config":{}}}}"#;

fn engine() -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn apply(e: &mut Engine, yaml: &str) {
    e.write_file("apply.yaml", yaml);
    let out = e.eval("kubectl apply -f apply.yaml");
    assert!(!out.starts_with("error"), "{out}");
}

fn informant(port_env: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: informant\n  namespace: homicide\n  labels: {{app: informant}}\nspec:\n  containers:\n  - name: c\n    image: informant:1\n    env: [{{name: PORT, value: \"{port_env}\"}}]\n"
    )
}

fn service(target_port: u16) -> String {
    format!(
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: informant\n  namespace: homicide\nspec:\n  selector: {{app: informant}}\n  ports:\n  - {{port: 80, targetPort: {target_port}}}\n"
    )
}

fn world(port_env: &str, target_port: u16) -> Engine {
    let mut e = engine();
    apply(&mut e, "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: homicide\n");
    apply(&mut e, &informant(port_env));
    apply(&mut e, &service(target_port));
    apply(
        &mut e,
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: toolbox\nspec:\n  containers:\n  - name: t\n    image: toolbox:1\n",
    );
    apply(
        &mut e,
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: toolbox\n  namespace: homicide\nspec:\n  containers:\n  - name: t\n    image: toolbox:1\n",
    );
    e.advance(10);
    e
}

#[test]
fn a_request_reaches_the_app_through_the_service() {
    let mut e = world("3000", 3000);
    for host in [
        "informant.homicide",
        "informant.homicide.svc",
        "informant.homicide.svc.cluster.local",
    ] {
        assert_eq!(
            e.eval(&format!("kubectl exec toolbox -- curl -s http://{host}")),
            "informant 1.2",
            "{host}"
        );
    }
    // From inside the namespace the short name is enough; from outside it is not.
    assert_eq!(
        e.eval("kubectl exec -n homicide toolbox -- curl -s informant/healthz"),
        "ok"
    );
    assert_eq!(
        e.eval("kubectl exec toolbox -- curl informant"),
        "curl: (6) Could not resolve host: informant\ncommand terminated with exit code 6"
    );
    assert_eq!(
        e.eval("kubectl exec toolbox -- curl -s -i http://informant.homicide/nope"),
        "HTTP/1.1 404 Not Found\nContent-Type: text/plain\nContent-Length: 9\n\nnot found"
    );
    assert_eq!(
        e.eval("kubectl exec toolbox -- curl -sf http://informant.homicide/nope"),
        "command terminated with exit code 22"
    );
}

#[test]
fn the_wrong_target_port_is_refused_and_a_wrong_service_port_times_out() {
    let mut e = world("3000", 8080);
    assert_eq!(
        e.eval("kubectl exec toolbox -- curl http://informant.homicide"),
        "curl: (7) Failed to connect to informant.homicide port 80 after 2 ms: Connection refused\ncommand terminated with exit code 7"
    );
    assert!(e
        .eval("kubectl exec toolbox -- curl http://informant.homicide:81")
        .starts_with("curl: (28)"));

    // Fixing the Service is enough: the app was always listening on 3000.
    apply(&mut e, &service(3000));
    assert_eq!(
        e.eval("kubectl exec toolbox -- curl -s http://informant.homicide"),
        "informant 1.2"
    );
}

#[test]
fn a_pod_that_is_not_ready_is_not_reachable_and_exec_needs_a_running_container() {
    let mut e = world("eighty", 3000);
    assert!(e
        .eval("kubectl exec toolbox -- curl http://informant.homicide")
        .starts_with("curl: (7)"));
    assert_eq!(
        e.eval("kubectl exec -n homicide informant -- curl localhost"),
        "error: unable to upgrade connection: container not found (\"c\")"
    );
    assert_eq!(
        e.eval("kubectl exec ghost -- curl x"),
        "Error from server (NotFound): pods \"ghost\" not found"
    );
}

#[test]
fn only_curl_exists_and_the_command_is_required() {
    let mut e = world("3000", 3000);
    assert_eq!(
        e.eval("kubectl exec toolbox -- ls /"),
        "error: running ls in a container is not available in this terminal (only curl is)"
    );
    assert_eq!(
        e.eval("kubectl exec toolbox"),
        "error: you must specify at least one command for the container"
    );
    assert!(e
        .eval("kubectl exec toolbox -- curl")
        .starts_with("curl: no URL specified"));
}
