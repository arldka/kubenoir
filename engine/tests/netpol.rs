//! End-to-end: NetworkPolicy decides what the oracle lets through (V10). A connection a policy
//! drops times out; one that is allowed is answered as usual.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"netpol","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"informant:1":{"pullSeconds":1,"app":"http-responder@0.2.0","config":{"name":"informant","version":"1.2","portEnv":"PORT"}},
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

const WORLD: &str = "\
apiVersion: v1
kind: Namespace
metadata: {name: homicide, labels: {zone: precinct}}
---
apiVersion: v1
kind: Pod
metadata: {name: informant, namespace: homicide, labels: {app: informant}}
spec:
  containers:
  - {name: c, image: 'informant:1', env: [{name: PORT, value: '3000'}]}
---
apiVersion: v1
kind: Service
metadata: {name: informant, namespace: homicide}
spec:
  selector: {app: informant}
  ports: [{port: 80, targetPort: 3000}]
---
apiVersion: v1
kind: Pod
metadata: {name: detective, namespace: homicide, labels: {role: detective}}
spec:
  containers: [{name: t, image: 'toolbox:1'}]
---
apiVersion: v1
kind: Pod
metadata: {name: stranger, namespace: homicide, labels: {role: stranger}}
spec:
  containers: [{name: t, image: 'toolbox:1'}]
---
apiVersion: v1
kind: Pod
metadata: {name: outsider}
spec:
  containers: [{name: t, image: 'toolbox:1'}]
";

const DENY_ALL: &str = "\
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: default-deny, namespace: homicide}
spec:
  podSelector: {}
  policyTypes: [Ingress]
";

fn world() -> Engine {
    let mut e = engine();
    apply(&mut e, WORLD);
    e.advance(10);
    e
}

fn curl(e: &mut Engine, from: &str, namespace: &str) -> String {
    e.eval(&format!(
        "kubectl exec -n {namespace} {from} -- curl -s http://informant.homicide"
    ))
}

#[test]
fn everything_flows_until_a_policy_selects_the_pod() {
    let mut e = world();
    assert_eq!(curl(&mut e, "detective", "homicide"), "informant 1.2");
    apply(&mut e, DENY_ALL);
    let blocked = curl(&mut e, "detective", "homicide");
    assert_eq!(blocked, "command terminated with exit code 28");
    assert!(e
        .eval("kubectl exec -n homicide detective -- curl http://informant.homicide")
        .starts_with("curl: (28)"));
    // Removing the policy opens the door again.
    e.eval("kubectl delete networkpolicy default-deny -n homicide");
    assert_eq!(curl(&mut e, "detective", "homicide"), "informant 1.2");
}

#[test]
fn an_allow_rule_names_who_gets_in_by_pod_labels_and_namespace_labels() {
    let mut e = world();
    apply(&mut e, DENY_ALL);
    apply(
        &mut e,
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: allow-detectives, namespace: homicide}
spec:
  podSelector: {matchLabels: {app: informant}}
  ingress:
  - from: [{podSelector: {matchLabels: {role: detective}}}]
    ports: [{port: 3000}]
",
    );
    assert_eq!(curl(&mut e, "detective", "homicide"), "informant 1.2");
    assert_eq!(
        curl(&mut e, "stranger", "homicide"),
        "command terminated with exit code 28"
    );
    // A podSelector alone stays inside the policy's namespace.
    assert_eq!(
        curl(&mut e, "outsider", "default"),
        "command terminated with exit code 28"
    );

    apply(
        &mut e,
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: allow-detectives, namespace: homicide}
spec:
  podSelector: {matchLabels: {app: informant}}
  ingress:
  - from: [{namespaceSelector: {matchLabels: {kubernetes.io/metadata.name: default}}}]
",
    );
    assert_eq!(curl(&mut e, "outsider", "default"), "informant 1.2");
    assert_eq!(
        curl(&mut e, "detective", "homicide"),
        "command terminated with exit code 28"
    );
}

#[test]
fn the_port_of_a_rule_is_the_port_the_pod_listens_on_not_the_service_port() {
    let mut e = world();
    apply(&mut e, DENY_ALL);
    let allow = |port: u16| {
        format!(
            "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {{name: allow, namespace: homicide}}
spec:
  podSelector: {{}}
  ingress:
  - ports: [{{port: {port}}}]
"
        )
    };
    apply(&mut e, &allow(80));
    assert_eq!(
        curl(&mut e, "detective", "homicide"),
        "command terminated with exit code 28"
    );
    apply(&mut e, &allow(3000));
    assert_eq!(curl(&mut e, "detective", "homicide"), "informant 1.2");
}

#[test]
fn egress_policies_hold_the_source_back() {
    let mut e = world();
    apply(
        &mut e,
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: quiet, namespace: homicide}
spec:
  podSelector: {matchLabels: {role: stranger}}
  policyTypes: [Egress]
  egress:
  - to: [{ipBlock: {cidr: 192.168.0.0/16}}]
",
    );
    assert_eq!(
        curl(&mut e, "stranger", "homicide"),
        "command terminated with exit code 28"
    );
    assert_eq!(curl(&mut e, "detective", "homicide"), "informant 1.2");
}

#[test]
fn policies_can_be_listed() {
    let mut e = world();
    apply(&mut e, DENY_ALL);
    let out = e.eval("kubectl get netpol -n homicide");
    let row: Vec<&str> = out.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(&row[..2], ["default-deny", "<none>"], "{out}");
}
