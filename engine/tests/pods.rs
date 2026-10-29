//! End-to-end: pods are immutable apart from a few fields; `replace --force` and `patch`.

use engine::{AppSet, Engine, Files};
use serde_json::Value;

const SCENARIO: &str =
    r#"{"schemaVersion":2,"id":"pods","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]}}"#;
const NODES: &str = "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n";

const INFORMANT: &str = "\
apiVersion: v1
kind: Pod
metadata:
  name: informant
spec:
  containers:
  - name: informant
    image: informant:1.0
    env:
    - name: TIP_SOURCE
      value: harbour
";

const WEB: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 1
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
      - name: web
        image: nginx:1.27
";

const FORBIDDEN: &str = "The Pod \"informant\" is invalid: spec: Forbidden: pod updates may not change fields other than `spec.containers[*].image`";

fn engine() -> Engine {
    let files = Files::from([("nodes.yaml".to_string(), NODES.to_string())]);
    let mut e = Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads");
    e.write_file("informant.yaml", INFORMANT);
    e.write_file("web.yaml", WEB);
    e
}

fn json(e: &mut Engine, what: &str) -> Value {
    serde_json::from_str(&e.eval(&format!("kubectl get {what} -o json"))).expect("json output")
}

fn tip(e: &mut Engine) -> Value {
    json(e, "pod informant")["spec"]["containers"][0]["env"][0]["value"].clone()
}

#[test]
fn a_running_pod_refuses_changes_to_its_spec() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl apply -f informant.yaml"), "pod/informant created");
    e.advance(10);
    e.write_file("informant.yaml", &INFORMANT.replace("harbour", "docks"));

    let out = e.eval("kubectl apply -f informant.yaml");
    assert!(
        out.starts_with("Error from server (Invalid): error when patching \"informant.yaml\": ")
            && out.contains(FORBIDDEN),
        "{out}"
    );
    let out = e.eval("kubectl replace -f informant.yaml");
    assert!(
        out.starts_with("Error from server (Invalid): error when replacing \"informant.yaml\": ")
            && out.contains(FORBIDDEN),
        "{out}"
    );
    let out = e.eval(r#"kubectl patch pod informant -p '{"spec":{"containers":[{"name":"informant","env":[{"name":"TIP_SOURCE","value":"docks"}]}]}}'"#);
    assert!(out.starts_with(FORBIDDEN), "{out}");
    assert_eq!(tip(&mut e), "harbour");
}

#[test]
fn the_image_and_added_tolerations_may_change() {
    let mut e = engine();
    e.eval("kubectl apply -f informant.yaml");
    e.advance(10);
    e.write_file("informant.yaml", &INFORMANT.replace("informant:1.0", "informant:2.0"));
    assert_eq!(e.eval("kubectl apply -f informant.yaml"), "pod/informant configured");
    assert_eq!(
        json(&mut e, "pod informant")["spec"]["containers"][0]["image"],
        "informant:2.0"
    );
    assert_eq!(
        e.eval(r#"kubectl patch pod informant --type json -p '[{"op":"add","path":"/spec/tolerations","value":[{"key":"night-shift","operator":"Exists"}]}]'"#),
        "pod/informant patched"
    );
}

#[test]
fn replace_force_deletes_and_recreates() {
    let mut e = engine();
    e.eval("kubectl apply -f informant.yaml");
    e.advance(10);
    let uid = json(&mut e, "pod informant")["metadata"]["uid"].clone();
    e.write_file("informant.yaml", &INFORMANT.replace("harbour", "docks"));
    assert_eq!(
        e.eval("kubectl replace --force -f informant.yaml"),
        "pod \"informant\" deleted\npod/informant replaced"
    );
    assert_eq!(tip(&mut e), "docks");
    assert_ne!(json(&mut e, "pod informant")["metadata"]["uid"], uid);
    e.advance(10);
    assert!(e.eval("kubectl get pod informant").contains("Running"));
}

#[test]
fn patch_types_on_a_deployment() {
    let mut e = engine();
    e.eval("kubectl apply -f web.yaml");
    assert_eq!(
        e.eval(r#"kubectl patch deployment web -p '{"spec":{"replicas":3}}'"#),
        "deployment.apps/web patched"
    );
    assert_eq!(
        e.eval(r#"kubectl patch deploy/web --type merge -p '{"spec":{"replicas":3}}'"#),
        "deployment.apps/web patched (no change)"
    );
    assert_eq!(
        e.eval(r#"kubectl patch deployment web --type json -p '[{"op":"replace","path":"/spec/replicas","value":2}]'"#),
        "deployment.apps/web patched"
    );
    let web = json(&mut e, "deployment web");
    assert_eq!(web["spec"]["replicas"], 2);
    assert_eq!(web["metadata"]["generation"], 3);

    // Strategic merge keeps the other containers' fields; YAML patches work too.
    assert_eq!(
        e.eval("kubectl patch deployment web -p 'spec: {template: {spec: {containers: [{name: web, image: nginx:1.28}]}}}'"),
        "deployment.apps/web patched"
    );
    let container = &json(&mut e, "deployment web")["spec"]["template"]["spec"]["containers"][0];
    assert_eq!(container["image"], "nginx:1.28");
    assert_eq!(container["imagePullPolicy"], "IfNotPresent");
}

#[test]
fn patch_errors() {
    let mut e = engine();
    e.eval("kubectl apply -f web.yaml");
    assert_eq!(
        e.eval(r#"kubectl patch deployment ghost -p '{"spec":{"replicas":3}}'"#),
        "Error from server (NotFound): deployments.apps \"ghost\" not found"
    );
    let out = e.eval(r#"kubectl patch deployment web -p '{"spec":{"selector":{"matchLabels":{"app":"other"}},"template":{"metadata":{"labels":{"app":"other"}}}}}'"#);
    assert!(
        out.starts_with("The Deployment \"web\" is invalid: spec.selector: Invalid value: v1.LabelSelector{MatchLabels:map[string]string{\"app\":\"other\"}"),
        "{out}"
    );
    assert!(out.ends_with("field is immutable"), "{out}");
    assert_eq!(
        e.eval("kubectl patch deployment web"),
        "error: must specify -p to patch"
    );
    assert_eq!(
        e.eval(r#"kubectl patch widgets w -p '{}'"#),
        "error: the server doesn't have a resource type \"widgets\""
    );
    assert_eq!(
        e.eval(r#"kubectl patch deployment web --type yaml -p '{}'"#),
        "error: --type must be one of [json merge strategic], not \"yaml\""
    );
    assert!(e
        .eval(r#"kubectl patch deployment web -p '{"spec": '"#)
        .starts_with("error: unable to parse \"{\"spec\": \": "));
}

#[test]
fn patch_and_replace_count_as_findings() {
    let scenario = r#"{"schemaVersion":2,"id":"pods","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
        "findings":[{"id":"scaled","clue":"more eyes on the street","when":{"command":{"verb":"patch","resource":"deploy","name":"web"}}},
                    {"id":"rewritten","clue":"a new story","when":{"command":{"verb":"replace","resource":"pod"}}}]}"#;
    let files = Files::from([("nodes.yaml".to_string(), NODES.to_string())]);
    let mut e = Engine::from_scenario(scenario, &files, AppSet::bundled()).unwrap();
    e.write_file("informant.yaml", INFORMANT);
    e.write_file("web.yaml", WEB);
    e.eval("kubectl apply -f web.yaml");
    e.eval("kubectl apply -f informant.yaml");
    let found = |e: &Engine| {
        serde_json::to_value(e.status()).unwrap()["findings"]["found"]
            .as_array()
            .unwrap()
            .len()
    };
    e.eval(r#"kubectl patch deployment ghost -p '{"spec":{"replicas":3}}'"#);
    assert_eq!(found(&e), 0);
    e.eval(r#"kubectl patch deployment web -p '{"spec":{"replicas":3}}'"#);
    assert_eq!(found(&e), 1);
    e.eval("kubectl replace --force -f informant.yaml");
    assert_eq!(found(&e), 2);
}
