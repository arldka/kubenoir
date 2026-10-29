//! End-to-end: probes. They are judged against what the program really does: a liveness probe
//! that fires before the app listens kills it, a readiness probe keeps a pod out of the
//! endpoints until it passes (V11, V18).

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"probes","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"web:1":{"pullSeconds":1,"app":"http-responder@0.2.0","config":{"name":"web","version":"1","portEnv":"PORT"}}}}"#;

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

/// A pod `web` that warms up for `warmup` seconds, with `probes` (YAML under the container).
fn pod(warmup: u32, probes: &str) -> String {
    format!(
        "apiVersion: v1
kind: Pod
metadata: {{name: web, labels: {{app: web}}}}
spec:
  containers:
  - name: web
    image: web:1
    env: [{{name: PORT, value: '8080'}}, {{name: WARMUP_SECS, value: '{warmup}'}}]
    ports: [{{name: http, containerPort: 8080}}]
{probes}"
    )
}

fn service() -> &'static str {
    "apiVersion: v1\nkind: Service\nmetadata: {name: web}\nspec:\n  selector: {app: web}\n  ports: [{port: 80, targetPort: http}]\n"
}

fn status(e: &mut Engine) -> String {
    e.eval("kubectl get pod web --no-headers")
}

fn endpoints(e: &mut Engine) -> String {
    e.eval("kubectl get endpoints web --no-headers")
}

fn restarts(e: &mut Engine) -> String {
    e.eval("kubectl get pod web -o jsonpath='{.status.containerStatuses[0].restartCount}'")
}

const LIVENESS: &str = "    livenessProbe:
      httpGet: {path: /healthz, port: 8080}
      periodSeconds: 10
      failureThreshold: 3
";

#[test]
fn a_liveness_probe_that_fires_before_the_app_listens_loops_the_container_with_clean_logs() {
    let mut e = engine();
    apply(&mut e, &pod(40, LIVENESS));
    e.advance(35);
    let events = e.eval("kubectl describe pod web");
    assert!(
        events.contains("Liveness probe failed: Get \"http://") && events.contains("connection refused"),
        "{events}"
    );
    assert!(
        events.contains("Container web failed liveness probe, will be restarted"),
        "{events}"
    );
    assert_ne!(restarts(&mut e), "0");
    let logs = e.eval("kubectl logs web");
    assert!(!logs.contains("ERROR") && !logs.contains("FATAL"), "{logs}");
    e.advance(300);
    // Without a readiness probe it looks ready between kills, as in a real cluster.
    assert!(status(&mut e).contains("Running"), "{}", status(&mut e));
    assert!(restarts(&mut e).parse::<u32>().unwrap() >= 10, "{}", restarts(&mut e));
}

#[test]
fn a_startup_probe_or_a_longer_delay_lets_the_slow_app_live() {
    for probes in [
        format!("{LIVENESS}    startupProbe:\n      httpGet: {{path: /healthz, port: 8080}}\n      periodSeconds: 5\n      failureThreshold: 20\n"),
        LIVENESS.replace("periodSeconds: 10", "periodSeconds: 10\n      initialDelaySeconds: 45"),
    ] {
        let mut e = engine();
        apply(&mut e, &pod(40, &probes));
        e.advance(120);
        assert_eq!(restarts(&mut e), "0", "{probes}\n{}", e.eval("kubectl describe pod web"));
        assert!(status(&mut e).contains("1/1"), "{}", status(&mut e));
    }
}

#[test]
fn readiness_keeps_a_pod_out_of_the_endpoints_until_it_passes() {
    let mut e = engine();
    apply(&mut e, service());
    apply(
        &mut e,
        &pod(
            5,
            "    readinessProbe:\n      httpGet: {path: /healthz, port: http}\n      initialDelaySeconds: 15\n",
        ),
    );
    e.advance(10);
    assert!(status(&mut e).contains("0/1"), "{}", status(&mut e));
    assert!(endpoints(&mut e).contains("<none>"), "{}", endpoints(&mut e));
    e.advance(10);
    assert!(status(&mut e).contains("1/1"), "{}", status(&mut e));
    assert!(endpoints(&mut e).contains(":8080"), "{}", endpoints(&mut e));
    assert_eq!(restarts(&mut e), "0");
}

#[test]
fn a_readiness_probe_that_keeps_failing_never_restarts_the_container() {
    let mut e = engine();
    apply(&mut e, service());
    apply(
        &mut e,
        &pod(0, "    readinessProbe:\n      httpGet: {path: /nope, port: 8080}\n"),
    );
    e.advance(200);
    assert!(status(&mut e).contains("0/1"), "{}", status(&mut e));
    assert_eq!(restarts(&mut e), "0");
    let events = e.eval("kubectl describe pod web");
    assert!(
        events.contains("Readiness probe failed: HTTP probe failed with statuscode: 404"),
        "{events}"
    );
    let held = e.eval("kubectl get endpoints web -o jsonpath='{.subsets[0].notReadyAddresses[0].ip}'");
    assert!(held.starts_with("10.244."), "{held}");
}

#[test]
fn tcp_and_exec_probes_ask_the_same_program() {
    for (probe, ready) in [
        ("tcpSocket: {port: 8080}", true),
        ("tcpSocket: {port: 9090}", false),
        ("exec: {command: [\"true\"]}", true),
        ("exec: {command: [cat, /tmp/healthy]}", false),
    ] {
        let mut e = engine();
        apply(&mut e, &pod(0, &format!("    readinessProbe:\n      {probe}\n")));
        e.advance(60);
        assert_eq!(status(&mut e).contains("1/1"), ready, "{probe}: {}", status(&mut e));
    }
}

#[test]
fn describe_shows_the_probes() {
    let mut e = engine();
    apply(&mut e, &pod(0, LIVENESS));
    e.advance(5);
    let out = e.eval("kubectl describe pod web");
    assert!(
        out.contains("Liveness:")
            && out.contains("http-get http://:8080/healthz delay=0s timeout=1s period=10s #success=1 #failure=3"),
        "{out}"
    );
}
