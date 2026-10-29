//! End-to-end: namespaces as real, cluster-scoped objects: created, described, listed, and
//! deleted with everything in them, and the other cluster-scoped kinds.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"namespaces","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["cluster.yaml"]},
    "success":{"assert":[{"label":"homicide still exists","select":{"kind":"Namespace","name":"homicide"},"each":[{"path":"status.phase","equals":"Active"}]}],
               "constraints":[{"label":"never delete the precinct","forbid":{"verb":"delete","resource":"namespace","name":"precinct"}}]}}"#;

const CLUSTER: &str = "\
apiVersion: v1
kind: Node
metadata:
  name: worker-1
---
apiVersion: v1
kind: Namespace
metadata:
  name: homicide
  labels:
    team: cid
  annotations:
    note: case file
---
apiVersion: v1
kind: Namespace
metadata:
  name: precinct
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: dossier
  namespace: homicide
---
apiVersion: v1
kind: Secret
metadata:
  name: alibi
  namespace: homicide
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: homicide
spec:
  replicas: 2
  selector:
    matchLabels: {app: web}
  template:
    metadata:
      labels: {app: web}
    spec:
      containers: [{name: web, image: 'nginx:1.27'}]
---
apiVersion: v1
kind: Pod
metadata:
  name: informant
  namespace: homicide
spec:
  containers: [{name: informant, image: 'informant:1.0'}]
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: elsewhere
  namespace: precinct
";

/// The cluster as the player finds it, no time having passed.
fn fresh() -> Engine {
    let files = Files::from([("cluster.yaml".to_string(), CLUSTER.to_string())]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

/// The same, after the pods have started.
fn engine() -> Engine {
    let mut e = fresh();
    e.advance(30);
    e
}

fn status(e: &Engine) -> serde_json::Value {
    serde_json::to_value(e.status()).unwrap()
}

#[test]
fn describe_namespace() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl describe namespace homicide"),
        "\
Name:         homicide
Labels:       kubernetes.io/metadata.name=homicide
              team=cid
Annotations:  note: case file
Status:       Active

No resource quota.

No LimitRange resource."
    );
    assert_eq!(
        e.eval("kubectl describe ns precinct").lines().nth(2),
        Some("Annotations:  <none>")
    );
    assert_eq!(
        e.eval("kubectl describe ns ghost"),
        "Error from server (NotFound): namespaces \"ghost\" not found"
    );
}

#[test]
fn deleting_a_namespace_empties_it_then_removes_it() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl delete namespace homicide"),
        "namespace \"homicide\" deleted"
    );
    // It is Terminating while the pods wind down.
    let listing = e.eval("kubectl get ns");
    assert!(
        listing
            .lines()
            .any(|l| l.starts_with("homicide") && l.contains("Terminating")),
        "{listing}"
    );
    assert!(e
        .eval("kubectl describe ns homicide")
        .contains("Status:       Terminating"));
    assert_eq!(
        e.eval("kubectl get cm -n homicide"),
        "No resources found in homicide namespace."
    );
    assert_eq!(
        e.eval("kubectl get deploy -n homicide"),
        "No resources found in homicide namespace."
    );

    e.advance(120);
    assert_eq!(
        e.eval("kubectl get ns homicide"),
        "Error from server (NotFound): namespaces \"homicide\" not found"
    );
    for kind in ["pods", "deploy", "rs", "cm", "secrets"] {
        assert_eq!(
            e.eval(&format!("kubectl get {kind} -n homicide")),
            "No resources found in homicide namespace.",
            "{kind}"
        );
    }
    // Other namespaces are untouched.
    assert_eq!(e.eval("kubectl get cm -n precinct -o name"), "configmap/elsewhere");
    assert_eq!(
        e.eval("kubectl delete namespace homicide"),
        "Error from server (NotFound): namespaces \"homicide\" not found"
    );
}

#[test]
fn a_terminating_namespace_accepts_nothing_new() {
    let mut e = engine();
    e.eval("kubectl delete ns homicide");
    assert_eq!(
        e.eval("kubectl run late --image=x -n homicide"),
        "Error from server (Forbidden): unable to create new content in namespace homicide because it is being terminated"
    );
    e.write_file(
        "late.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: late\n  namespace: homicide\n",
    );
    assert_eq!(
        e.eval("kubectl apply -f late.yaml"),
        "Error from server (Forbidden): error when creating \"late.yaml\": unable to create new content in namespace homicide because it is being terminated"
    );
}

#[test]
fn the_system_namespaces_cannot_be_deleted() {
    let mut e = engine();
    for ns in ["default", "kube-system", "kube-public"] {
        assert_eq!(
            e.eval(&format!("kubectl delete namespace {ns}")),
            format!(
                "Error from server (Forbidden): namespaces \"{ns}\" is forbidden: this namespace may not be deleted"
            )
        );
    }
    assert_eq!(e.eval("kubectl get ns default -o name"), "namespace/default");
    // kube-node-lease may go, like in a real cluster.
    assert_eq!(
        e.eval("kubectl delete ns kube-node-lease"),
        "namespace \"kube-node-lease\" deleted"
    );
    e.advance(5);
    assert_eq!(e.eval("kubectl get ns -o name | grep -c lease"), "0");
}

#[test]
fn namespaces_can_be_deleted_from_a_file() {
    let mut e = engine();
    e.write_file(
        "ns.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: precinct\n",
    );
    assert_eq!(e.eval("kubectl delete -f ns.yaml"), "namespace \"precinct\" deleted");
    e.advance(5);
    assert_eq!(
        e.eval("kubectl get ns precinct -o name"),
        "Error from server (NotFound): namespaces \"precinct\" not found"
    );
    e.write_file(
        "sys.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: kube-system\n",
    );
    assert!(e
        .eval("kubectl delete -f sys.yaml")
        .contains("this namespace may not be deleted"));
}

#[test]
fn delete_works_for_every_kind_and_names_what_is_missing() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl delete cm dossier -n homicide"),
        "configmap \"dossier\" deleted"
    );
    assert_eq!(
        e.eval("kubectl delete secret alibi -n homicide"),
        "secret \"alibi\" deleted"
    );
    assert_eq!(
        e.eval("kubectl delete cm dossier -n homicide"),
        "Error from server (NotFound): configmaps \"dossier\" not found"
    );
    assert_eq!(e.eval("kubectl delete node worker-1"), "node \"worker-1\" deleted");
    assert_eq!(e.eval("kubectl get nodes"), "No resources found");
    assert_eq!(
        e.eval("kubectl delete node worker-1"),
        "Error from server (NotFound): nodes \"worker-1\" not found"
    );
    // -n does not apply to cluster-scoped kinds.
    assert_eq!(
        e.eval("kubectl delete ns nope -n homicide"),
        "Error from server (NotFound): namespaces \"nope\" not found"
    );
    assert_eq!(
        e.eval("kubectl delete widgets x"),
        "error: the server doesn't have a resource type \"widgets\""
    );
}

#[test]
fn the_judge_sees_namespace_deletes() {
    let mut clean = fresh();
    clean.advance(30);
    assert_eq!(status(&clean)["outcome"], "closed");

    let mut e = fresh();
    e.eval("kubectl delete ns precinct");
    e.advance(30);
    // The forbidden delete broke the constraint; the case still closes, at a cost.
    let status = status(&e);
    assert_eq!(status["solved"], true);
    assert_eq!(status["outcome"], "closedWithCost");
}

#[test]
fn completion_knows_namespaces_and_their_contents() {
    let e = engine();
    assert_eq!(e.complete("kubectl get pods -n h").candidates, ["homicide"]);
    assert_eq!(
        e.complete("kubectl get ns ").candidates,
        [
            "default",
            "homicide",
            "kube-node-lease",
            "kube-public",
            "kube-system",
            "precinct"
        ]
    );
    assert_eq!(e.complete("kubectl delete cm -n homicide ").candidates, ["dossier"]);
    assert_eq!(e.complete("kubectl get node ").candidates, ["worker-1"]);
}
