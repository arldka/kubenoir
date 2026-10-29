//! End-to-end: Services get cluster IPs and node ports, and the Endpoints and EndpointSlices that
//! follow the Ready pods their selector matches (V11).

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"services","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"web:1":{"pullSeconds":1,"app":"nginx@0.1.0","config":{}},
              "bad:1":{"pullSeconds":1,"app":"script@0.1.0","config":{"lines":["boom"],"exit":1}}}}"#;

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

fn pod(name: &str, namespace: &str, labels: &str, image: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {name}\n  namespace: {namespace}\n  labels: {labels}\nspec:\n  containers:\n  - name: c\n    image: {image}\n    ports:\n    - {{name: http, containerPort: 8080}}\n"
    )
}

fn service(name: &str, namespace: &str, extra_spec: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: {name}\n  namespace: {namespace}\nspec:\n  selector: {{app: web}}\n  ports:\n  - {{port: 80, targetPort: http}}\n{extra_spec}"
    )
}

fn namespace(name: &str) -> String {
    format!("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {name}\n")
}

fn endpoints(e: &mut Engine, name: &str) -> String {
    e.eval(&format!("kubectl get endpoints {name} --no-headers"))
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn services_get_distinct_cluster_ips_and_node_ports() {
    let mut e = engine();
    apply(&mut e, &service("a", "default", ""));
    apply(&mut e, &service("b", "default", "  type: NodePort\n"));
    apply(&mut e, &service("c", "default", "  type: NodePort\n"));
    let ips = e.eval("kubectl get svc -o jsonpath='{.items[*].spec.clusterIP}'");
    let ips: Vec<&str> = ips.split(' ').collect();
    assert_eq!(ips.len(), 3, "{ips:?}");
    assert!(ips.iter().all(|ip| ip.starts_with("10.96.")), "{ips:?}");
    assert!(ips[0] != ips[1] && ips[1] != ips[2] && ips[0] != ips[2], "{ips:?}");
    assert_eq!(
        e.eval("kubectl get svc b c -o jsonpath='{.items[*].spec.ports[0].nodePort}'"),
        "30000 30001"
    );
    assert!(e.eval("kubectl get svc").contains("80:30000/TCP"));
    // The address is kept when the Service is applied again.
    let before = e.eval("kubectl get svc a -o jsonpath='{.spec.clusterIP}'");
    apply(&mut e, &service("a", "default", "  sessionAffinity: None\n"));
    assert_eq!(e.eval("kubectl get svc a -o jsonpath='{.spec.clusterIP}'"), before);
}

#[test]
fn only_ready_matching_pods_in_the_namespace_are_endpoints() {
    let mut e = engine();
    apply(&mut e, &namespace("other"));
    apply(&mut e, &service("web", "default", ""));
    assert_eq!(endpoints(&mut e, "web"), "<none>");

    apply(&mut e, &pod("one", "default", "{app: web}", "web:1"));
    apply(&mut e, &pod("two", "default", "{app: web}", "web:1"));
    apply(&mut e, &pod("wrong", "default", "{app: other}", "web:1"));
    apply(&mut e, &pod("abroad", "other", "{app: web}", "web:1"));
    apply(&mut e, &pod("crash", "default", "{app: web}", "bad:1"));
    e.advance(20);

    let one = e.eval("kubectl get pod one -o jsonpath='{.status.podIP}'");
    let two = e.eval("kubectl get pod two -o jsonpath='{.status.podIP}'");
    let mut want = [format!("{one}:8080"), format!("{two}:8080")];
    want.sort();
    assert_eq!(endpoints(&mut e, "web"), want.join(","));

    // The crashing pod matches but is never ready, so it only shows as not ready.
    let ep = e.eval("kubectl get endpoints web -o jsonpath='{.subsets[0].notReadyAddresses[*].targetRef.name}'");
    assert!(ep.is_empty() || ep == "crash", "{ep}");

    // A terminating pod leaves at once.
    e.eval("kubectl delete pod one");
    assert_eq!(endpoints(&mut e, "web"), format!("{two}:8080"));
}

#[test]
fn a_selector_typo_leaves_the_endpoints_empty_and_describe_says_so() {
    let mut e = engine();
    apply(&mut e, &pod("one", "default", "{app: web}", "web:1"));
    apply(&mut e, &service("web", "default", "").replace("app: web", "app: webb"));
    e.advance(10);
    assert_eq!(endpoints(&mut e, "web"), "<none>");
    let describe = e.eval("kubectl describe service web");
    assert!(describe.contains("Selector:          app=webb"), "{describe}");
    assert!(describe.contains("Endpoints:         <none>"), "{describe}");

    // Fixing the selector brings the pod in.
    apply(&mut e, &service("web", "default", ""));
    let ip = e.eval("kubectl get pod one -o jsonpath='{.status.podIP}'");
    assert_eq!(endpoints(&mut e, "web"), format!("{ip}:8080"));
    let describe = e.eval("kubectl describe svc web");
    assert!(
        describe.contains(&format!("Endpoints:         {ip}:8080")),
        "{describe}"
    );
    assert!(describe.contains("TargetPort:        http/TCP"), "{describe}");
}

#[test]
fn endpoint_slices_follow_and_go_with_their_service() {
    let mut e = engine();
    apply(&mut e, &service("web", "default", ""));
    apply(&mut e, &pod("one", "default", "{app: web}", "web:1"));
    e.advance(10);
    let slices = e.eval("kubectl get endpointslices --no-headers");
    let row: Vec<&str> = slices.split_whitespace().collect();
    assert!(row[0].starts_with("web-"), "{slices}");
    assert_eq!(
        &row[1..4],
        [
            "IPv4",
            "8080",
            &e.eval("kubectl get pod one -o jsonpath='{.status.podIP}'")
        ]
    );
    assert_eq!(
        e.eval("kubectl get endpointslices -l kubernetes.io/service-name=web --no-headers")
            .lines()
            .count(),
        1
    );

    e.eval("kubectl delete service web");
    assert_eq!(
        e.eval("kubectl get endpoints"),
        "No resources found in default namespace."
    );
    assert_eq!(
        e.eval("kubectl get endpointslices"),
        "No resources found in default namespace."
    );
}

#[test]
fn a_service_without_a_selector_keeps_the_endpoints_somebody_wrote() {
    let mut e = engine();
    apply(
        &mut e,
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: db\nspec:\n  ports:\n  - {port: 5432}\n",
    );
    apply(
        &mut e,
        "apiVersion: v1\nkind: Endpoints\nmetadata:\n  name: db\nsubsets:\n- addresses: [{ip: 192.168.1.10}]\n  ports: [{port: 5432}]\n",
    );
    e.advance(10);
    assert_eq!(endpoints(&mut e, "db"), "192.168.1.10:5432");
}
