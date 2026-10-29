//! End-to-end: images that cannot be pulled (`ErrImagePull`, `ImagePullBackOff`) and the pull
//! secrets that fix the ones behind a login.

use engine::{AppSet, Engine, Files};

fn scenario(catalog: &str) -> String {
    format!(
        r#"{{"schemaVersion":2,"id":"pull","imageCatalog":"{catalog}","cluster":{{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]}},
        "images":{{
            "web:1":{{"app":"nginx@0.1.0"}},
            "web:2":{{"app":"nginx@0.1.0"}},
            "registry.example.com/badge:1":{{"app":"nginx@0.1.0","auth":{{"username":"badge","password":"1234"}}}},
            "gone:1":{{"exists":false}}}}}}"#
    )
}

fn engine_with(catalog: &str) -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(&scenario(catalog), &files, AppSet::bundled()).expect("scenario loads")
}

fn engine() -> Engine {
    engine_with("closed")
}

fn status(e: &mut Engine, pod: &str) -> String {
    e.eval(&format!("kubectl get pod {pod} --no-headers"))
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .to_string()
}

fn events(e: &mut Engine) -> String {
    e.eval("kubectl get events")
}

#[test]
fn a_mistyped_tag_fails_to_pull_backs_off_and_recovers_when_fixed() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl run p --image=web:1l"), "pod/p created");
    e.advance(3);
    assert_eq!(status(&mut e, "p"), "ErrImagePull");
    assert_eq!(
        e.eval("kubectl get pod p -o jsonpath='{.status.containerStatuses[0].state.waiting.message}'"),
        "Failed to pull image \"web:1l\": rpc error: code = NotFound desc = failed to pull and unpack image \"docker.io/library/web:1l\": failed to resolve reference \"docker.io/library/web:1l\": docker.io/library/web:1l: not found"
    );
    e.advance(5);
    assert_eq!(status(&mut e, "p"), "ImagePullBackOff");
    assert_eq!(
        e.eval("kubectl get pod p -o jsonpath='{.status.containerStatuses[0].state.waiting.message}'"),
        "Back-off pulling image \"web:1l\""
    );
    let events = events(&mut e);
    for expected in [
        "Pulling",
        "Failed to pull image \"web:1l\"",
        "Error: ErrImagePull",
        "Back-off pulling image \"web:1l\"",
        "Error: ImagePullBackOff",
    ] {
        assert!(events.contains(expected), "{expected}\n{events}");
    }
    // It never ran: no logs, no restarts, and the pod is still Pending.
    assert_eq!(
        e.eval("kubectl get pod p --no-headers").split_whitespace().nth(3),
        Some("0")
    );
    assert_eq!(e.eval("kubectl get pod p -o jsonpath='{.status.phase}'"), "Pending");
    assert!(e.eval("kubectl logs p").starts_with("Error from server (BadRequest)"));

    // Retries back off: 10 s, then 20 s, then 40 s apart. The failure repeats, never runs.
    e.advance(100);
    assert!(["ErrImagePull", "ImagePullBackOff"].contains(&status(&mut e, "p").as_str()));
    assert!(
        events_count(&mut e, "Error: ErrImagePull") >= 3,
        "{}",
        self::events(&mut e)
    );

    // Fixing the image takes effect on the next attempt, with no new pod.
    assert_eq!(e.eval("kubectl set image pod/p p=web:1"), "pod/p image updated");
    e.advance(150);
    assert_eq!(status(&mut e, "p"), "Running");
    assert!(self::events(&mut e).contains("Successfully pulled image \"web:1\""));
}

/// How many times an event happened (its `count`, or 1).
fn events_count(e: &mut Engine, message: &str) -> i64 {
    let json: serde_json::Value = serde_json::from_str(&e.eval("kubectl get events -o json")).unwrap();
    json["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["message"] == message)
        .map(|i| i["count"].as_i64().unwrap_or(1))
        .sum()
}

#[test]
fn a_deployment_stuck_on_a_bad_image_rolls_out_once_the_image_is_fixed() {
    let mut e = engine();
    e.eval("kubectl create deployment web --image=web:1l");
    e.advance(30);
    let pods = e.eval("kubectl get pods --no-headers");
    assert!(
        pods.contains("ErrImagePull") || pods.contains("ImagePullBackOff"),
        "{pods}"
    );
    let rollout = e.eval("kubectl rollout status deployment/web");
    assert!(
        rollout.starts_with("Waiting for deployment \"web\" rollout to finish"),
        "{rollout}"
    );
    assert!(e
        .eval("kubectl describe deployment web")
        .contains("MinimumReplicasUnavailable"));

    assert_eq!(
        e.eval("kubectl set image deployment/web web=web:1"),
        "deployment.apps/web image updated"
    );
    e.advance(120);
    assert_eq!(
        e.eval("kubectl rollout status deployment/web"),
        "deployment \"web\" successfully rolled out"
    );
    let pods = e.eval("kubectl get pods --no-headers");
    assert_eq!(pods.lines().count(), 1, "the stuck pod was replaced: {pods}");
    assert!(pods.contains("Running"));
}

#[test]
fn describe_shows_the_waiting_reason_and_the_failed_pulls() {
    let mut e = engine();
    e.eval("kubectl run p --image=gone:1");
    e.advance(8);
    let describe = e.eval("kubectl describe pod p");
    assert!(
        describe.contains("State:          Waiting\n      Reason:       ImagePullBackOff"),
        "{describe}"
    );
    assert!(describe.contains("Warning  Failed"), "{describe}");
    assert!(describe.contains("Normal   BackOff"), "{describe}");
}

const PRIVATE_POD: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: badge\nspec:\n  imagePullSecrets:\n  - name: pull\n  containers:\n  - name: badge\n    image: registry.example.com/badge:1\n";

#[test]
fn a_private_image_needs_a_pull_secret_with_the_right_credentials() {
    let mut e = engine();
    // No secret at all.
    e.write_file("badge.yaml", PRIVATE_POD);
    assert_eq!(e.eval("kubectl apply -f badge.yaml"), "pod/badge created");
    e.advance(5);
    assert_eq!(status(&mut e, "badge"), "ErrImagePull");
    assert!(events(&mut e).contains("pull access denied, repository does not exist or may require authorization"));

    // A secret with the wrong password: still refused.
    e.eval("kubectl create secret docker-registry pull --docker-server=registry.example.com --docker-username=badge --docker-password=wrong");
    e.advance(100);
    assert_ne!(status(&mut e, "badge"), "Running");

    // The wrong registry: refused.
    e.eval("kubectl delete secret pull");
    e.eval("kubectl create secret docker-registry pull --docker-server=other.example.com --docker-username=badge --docker-password=1234");
    e.advance(200);
    assert_ne!(status(&mut e, "badge"), "Running");

    // The right one: the pod starts at its next attempt, with no new pod.
    e.eval("kubectl delete secret pull");
    assert_eq!(
        e.eval("kubectl create secret docker-registry pull --docker-server=registry.example.com --docker-username=badge --docker-password=1234"),
        "secret/pull created"
    );
    e.advance(400);
    assert_eq!(status(&mut e, "badge"), "Running");
}

#[test]
fn a_secret_the_pod_does_not_name_does_not_help() {
    let mut e = engine();
    e.eval("kubectl create secret docker-registry pull --docker-server=registry.example.com --docker-username=badge --docker-password=1234");
    e.eval("kubectl run badge --image=registry.example.com/badge:1");
    e.advance(60);
    assert_ne!(status(&mut e, "badge"), "Running");
    // An opaque secret of the same name is no registry login either.
    e.write_file(
        "badge.yaml",
        PRIVATE_POD.replace("name: badge\nspec", "name: badge2\nspec").as_str(),
    );
    e.eval("kubectl delete secret pull");
    e.eval("kubectl create secret generic pull --from-literal=a=b");
    e.eval("kubectl apply -f badge.yaml");
    e.advance(60);
    assert_ne!(status(&mut e, "badge2"), "Running");
}

#[test]
fn a_pull_secret_can_be_given_through_a_deployment() {
    let mut e = engine();
    e.eval("kubectl create deployment badge --image=registry.example.com/badge:1");
    e.eval("kubectl create secret docker-registry pull --docker-server=registry.example.com --docker-username=badge --docker-password=1234");
    e.advance(30);
    assert!(!e.eval("kubectl get pods --no-headers").contains("Running"));
    e.eval(
        r#"kubectl patch deployment badge -p '{"spec":{"template":{"spec":{"imagePullSecrets":[{"name":"pull"}]}}}}'"#,
    );
    e.advance(120);
    assert_eq!(
        e.eval("kubectl rollout status deployment/badge"),
        "deployment \"badge\" successfully rolled out"
    );
}

#[test]
fn an_open_catalog_pulls_what_it_does_not_list_but_not_what_does_not_exist() {
    let mut e = engine_with("open");
    e.eval("kubectl run anything --image=whatever:7");
    e.eval("kubectl run missing --image=gone:1");
    e.advance(8);
    assert_eq!(status(&mut e, "anything"), "Running");
    assert_ne!(status(&mut e, "missing"), "Running");

    let mut closed = engine_with("closed");
    closed.eval("kubectl run anything --image=whatever:7");
    closed.advance(8);
    assert_ne!(status(&mut closed, "anything"), "Running");
}

#[test]
fn an_unknown_catalog_setting_is_a_load_error() {
    let files = Files::new();
    let bad = r#"{"schemaVersion":2,"id":"x","imageCatalog":"sometimes","cluster":{"now":"2026-10-27T18:30:00Z"}}"#;
    let err = Engine::from_scenario(bad, &files, AppSet::bundled())
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("imageCatalog") || err.contains("sometimes") || err.contains("unknown variant"),
        "{err}"
    );
}
