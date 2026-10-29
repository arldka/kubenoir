//! End-to-end: `kubectl apply|create|delete -f` against files written into the engine.

use engine::{AppSet, Engine, Files};
use serde_json::Value;

const SCENARIO: &str =
    r#"{"schemaVersion":2,"id":"apply","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]}}"#;
const NODES: &str = "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n";

const WEB: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    team: precinct
spec:
  replicas: 2
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

fn engine() -> Engine {
    let files = Files::from([("nodes.yaml".to_string(), NODES.to_string())]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn json(e: &mut Engine, what: &str) -> Value {
    serde_json::from_str(&e.eval(&format!("kubectl get {what} -o json"))).expect("json output")
}

#[test]
fn apply_creates_then_reports_unchanged_without_a_new_generation() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    assert_eq!(e.eval("kubectl apply -f web.yaml"), "deployment.apps/web created");
    let first = json(&mut e, "deployment web");
    assert_eq!(first["metadata"]["generation"], 1);
    assert!(
        first["metadata"]["annotations"]["kubectl.kubernetes.io/last-applied-configuration"]
            .as_str()
            .unwrap()
            .contains("\"replicas\":2")
    );

    assert_eq!(e.eval("kubectl apply -f ./web.yaml"), "deployment.apps/web unchanged");
    let second = json(&mut e, "deployment web");
    assert_eq!(second["metadata"]["generation"], 1);
    assert_eq!(
        second["metadata"]["resourceVersion"],
        first["metadata"]["resourceVersion"]
    );
}

#[test]
fn applied_deployments_are_run_by_the_controllers() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    e.eval("kubectl apply -f web.yaml");
    e.advance(30);
    let pods = e.eval("kubectl get pods");
    assert_eq!(pods.lines().filter(|l| l.starts_with("web-")).count(), 2, "{pods}");
}

#[test]
fn apply_configures_changes_and_drops_removed_fields() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    e.eval("kubectl apply -f web.yaml");
    e.write_file(
        "web.yaml",
        &WEB.replace("replicas: 2", "replicas: 3")
            .replace("  labels:\n    team: precinct\n", ""),
    );
    assert_eq!(e.eval("kubectl apply -f web.yaml"), "deployment.apps/web configured");
    let web = json(&mut e, "deployment web");
    assert_eq!(web["spec"]["replicas"], 3);
    assert_eq!(web["metadata"]["generation"], 2);
    assert!(web["metadata"].get("labels").is_none(), "{}", web["metadata"]);
}

#[test]
fn apply_over_create_warns_about_the_missing_annotation() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    assert_eq!(e.eval("kubectl create -f web.yaml"), "deployment.apps/web created");
    let out = e.eval("kubectl apply -f web.yaml");
    assert!(
        out.starts_with("Warning: resource deployments/web is missing the kubectl.kubernetes.io/last-applied-configuration annotation"),
        "{out}"
    );
    assert!(out.ends_with("\ndeployment.apps/web configured"), "{out}");
}

#[test]
fn create_refuses_existing_objects() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    e.eval("kubectl create -f web.yaml");
    assert_eq!(
        e.eval("kubectl create -f web.yaml"),
        "Error from server (AlreadyExists): error when creating \"web.yaml\": deployments.apps \"web\" already exists"
    );
}

#[test]
fn multi_document_files_and_lists_in_order() {
    let mut e = engine();
    e.write_file(
        "case.yaml",
        "\
apiVersion: v1
kind: Namespace
metadata:
  name: homicide
---
apiVersion: v1
kind: List
items:
- apiVersion: v1
  kind: ConfigMap
  metadata:
    name: dossier
    namespace: homicide
  data:
    suspect: Moriarty
- apiVersion: v1
  kind: Pod
  metadata:
    name: informant
    namespace: homicide
  spec:
    containers:
    - name: informant
      image: informant:1.0
",
    );
    assert_eq!(
        e.eval("kubectl apply -f case.yaml"),
        "namespace/homicide created\nconfigmap/dossier created\npod/informant created"
    );
    assert_eq!(e.eval("kubectl get pods -n homicide").lines().count(), 2);
}

#[test]
fn server_errors_name_the_file_and_come_after_successes() {
    let mut e = engine();
    e.write_file(
        "mixed.yaml",
        "\
apiVersion: v1
kind: Pod
metadata:
  name: bad
spec:
  containers:
  - name: app
    imagee: nginx
---
apiVersion: v1
kind: Pod
metadata:
  name: lost
  namespace: homicide
spec:
  containers:
  - name: app
    image: nginx
---
apiVersion: v1
kind: Pod
metadata:
  name: good
spec:
  containers:
  - name: app
    image: nginx
---
apiVersion: example.com/v1
kind: Widget
metadata:
  name: w
",
    );
    assert_eq!(
        e.eval("kubectl apply -f mixed.yaml"),
        "\
pod/good created
Error from server (BadRequest): error when creating \"mixed.yaml\": Pod in version \"v1\" cannot be handled as a Pod: strict decoding error: unknown field \"spec.containers[0].imagee\"
Error from server (NotFound): error when creating \"mixed.yaml\": namespaces \"homicide\" not found
error: resource mapping not found for name: \"w\" namespace: \"\" from \"mixed.yaml\": no matches for kind \"Widget\" in version \"example.com/v1\"
ensure CRDs are installed first"
    );
}

#[test]
fn invalid_documents_stop_the_whole_command() {
    let mut e = engine();
    e.write_file("half.yaml", &format!("{WEB}---\nmetadata:\n  name: x\n"));
    assert_eq!(
        e.eval("kubectl apply -f half.yaml"),
        "error: error validating \"half.yaml\": error validating data: [apiVersion not set, kind not set]; if you choose to ignore these errors, turn validation off with --validate=false"
    );
    assert_eq!(
        e.eval("kubectl get deployments"),
        "No resources found in default namespace."
    );

    e.write_file("broken.yaml", "kind: [unclosed");
    assert!(e
        .eval("kubectl apply -f broken.yaml")
        .starts_with("error: error parsing broken.yaml: error converting YAML to JSON: "));
}

#[test]
fn namespace_flag_and_object_namespace_must_agree() {
    let mut e = engine();
    e.write_file(
        "web.yaml",
        &WEB.replace("  name: web\n", "  name: web\n  namespace: kube-system\n"),
    );
    assert_eq!(
        e.eval("kubectl apply -f web.yaml -n default"),
        "error: the namespace from the provided object \"kube-system\" does not match the namespace \"default\". You must pass '--namespace=kube-system' to perform this operation."
    );
    assert_eq!(e.eval("kubectl apply -f web.yaml"), "deployment.apps/web created");
    e.write_file("plain.yaml", WEB);
    assert_eq!(
        e.eval("kubectl apply -n kube-public -f plain.yaml"),
        "deployment.apps/web created"
    );
    assert_eq!(
        json(&mut e, "deployment web -n kube-public")["metadata"]["namespace"],
        "kube-public"
    );
}

#[test]
fn delete_by_file() {
    let mut e = engine();
    e.write_file("web.yaml", WEB);
    e.eval("kubectl apply -f web.yaml");
    e.advance(30);
    assert_eq!(e.eval("kubectl delete -f web.yaml"), "deployment.apps \"web\" deleted");
    assert_eq!(
        e.eval("kubectl delete -f web.yaml"),
        "Error from server (NotFound): error when deleting \"web.yaml\": deployments.apps \"web\" not found"
    );
    assert_eq!(
        e.eval("kubectl get replicasets"),
        "No resources found in default namespace."
    );
}

#[test]
fn flags_kubectl_rejects_or_this_terminal_does_not_offer() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl apply"), "error: must specify one of -f and -k");
    assert_eq!(e.eval("kubectl apply web.yaml"), "error: Unexpected args: [web.yaml]");
    assert_eq!(
        e.eval("kubectl apply -k overlays/prod"),
        "error: kustomize (-k) is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl apply -f -"),
        "error: reading manifests from stdin (-f -) is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl delete pod web -f web.yaml"),
        "error: resource names and -f cannot be used together"
    );
    assert_eq!(
        e.eval("kubectl apply -f missing.yaml"),
        "error: the path \"missing.yaml\" does not exist"
    );
}
