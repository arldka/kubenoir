//! End-to-end: `kubectl get` for every kind, with selectors, scopes and output formats.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"get","cluster":{"startedAt":"2026-10-27T18:00:00Z","now":"2026-10-27T18:30:00Z","manifests":["cluster.yaml"]}}"#;

const CLUSTER: &str = "\
apiVersion: v1
kind: Node
metadata:
  name: control-plane
  labels:
    node-role.kubernetes.io/control-plane: ''
---
apiVersion: v1
kind: Node
metadata:
  name: worker-1
spec:
  unschedulable: true
status:
  addresses:
  - {type: InternalIP, address: 192.168.49.3}
  nodeInfo:
    kubeletVersion: v1.36.2
    osImage: Debian GNU/Linux 12
    kernelVersion: 6.1.0
    containerRuntimeVersion: containerd://2.0.0
    machineID: ''
    systemUUID: ''
    bootID: ''
    architecture: amd64
    operatingSystem: linux
    kubeProxyVersion: v1.36.2
  conditions:
  - {type: Ready, status: 'False'}
---
apiVersion: v1
kind: Namespace
metadata:
  name: homicide
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: dossier
  namespace: homicide
data:
  suspect: Moriarty
  motive: money
binaryData:
  photo: AAEC
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: empty
---
apiVersion: v1
kind: Secret
metadata:
  name: alibi
  namespace: homicide
data:
  password: cGFzcw==
---
apiVersion: v1
kind: Service
metadata:
  name: switchboard
  namespace: homicide
  labels: {team: radio}
spec:
  clusterIP: 10.96.0.20
  selector: {app: informant, tier: front}
  ports:
  - {port: 80, targetPort: 8080}
  - {port: 443, name: tls}
---
apiVersion: v1
kind: Service
metadata:
  name: outside
spec:
  type: ExternalName
  externalName: example.com
  ports:
  - port: 80
---
apiVersion: v1
kind: Service
metadata:
  name: front-door
spec:
  type: LoadBalancer
  ports:
  - {port: 80, nodePort: 30080, protocol: TCP}
---
apiVersion: v1
kind: Pod
metadata:
  name: informant
  namespace: homicide
  labels: {app: informant, tier: front}
spec:
  containers: [{name: informant, image: 'informant:1.0'}]
---
apiVersion: v1
kind: Pod
metadata:
  name: witness
  labels: {app: witness}
spec:
  containers: [{name: witness, image: 'informant:1.0'}]
";

fn engine() -> Engine {
    let files = Files::from([("cluster.yaml".to_string(), CLUSTER.to_string())]);
    let mut e = Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads");
    e.advance(10);
    e
}

#[test]
fn nodes() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get nodes"),
        "\
NAME            STATUS                        ROLES           AGE   VERSION
control-plane   Ready                         control-plane   30m   v1.36.0
worker-1        NotReady,SchedulingDisabled   <none>          30m   v1.36.2"
    );
    assert_eq!(
        e.eval("kubectl get node worker-1 -o wide"),
        "\
NAME       STATUS                        ROLES    AGE   VERSION   INTERNAL-IP    EXTERNAL-IP   OS-IMAGE              KERNEL-VERSION   CONTAINER-RUNTIME
worker-1   NotReady,SchedulingDisabled   <none>   30m   v1.36.2   192.168.49.3   <none>        Debian GNU/Linux 12   6.1.0            containerd://2.0.0"
    );
    // Nodes are not namespaced: -n changes nothing.
    assert_eq!(
        e.eval("kubectl get nodes -n homicide -o name"),
        "node/control-plane\nnode/worker-1"
    );
    assert_eq!(
        e.eval("kubectl get node nope"),
        "Error from server (NotFound): nodes \"nope\" not found"
    );
}

#[test]
fn namespaces() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get ns"),
        "\
NAME              STATUS   AGE
default           Active   30m
homicide          Active   30m
kube-node-lease   Active   30m
kube-public       Active   30m
kube-system       Active   30m"
    );
    assert_eq!(e.eval("kubectl get namespace homicide -o name"), "namespace/homicide");
    assert_eq!(
        e.eval("kubectl get ns homicide -o jsonpath='{.metadata.labels}'"),
        "{\"kubernetes.io/metadata.name\":\"homicide\"}"
    );
}

#[test]
fn configmaps_secrets_and_services() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get cm -n homicide"),
        "NAME      DATA   AGE\ndossier   3      30m"
    );
    assert_eq!(e.eval("kubectl get cm"), "NAME    DATA   AGE\nempty   0      30m");
    assert_eq!(
        e.eval("kubectl get secrets -n homicide"),
        "NAME    TYPE     DATA   AGE\nalibi   Opaque   1      30m"
    );
    assert_eq!(
        e.eval("kubectl get svc"),
        "\
NAME         TYPE           CLUSTER-IP   EXTERNAL-IP   PORT(S)        AGE
front-door   LoadBalancer   10.96.0.10   <pending>     80:30080/TCP   30m
outside      ExternalName   <none>       example.com   80/TCP         30m"
    );
    assert_eq!(
        e.eval("kubectl get service switchboard -n homicide -o wide"),
        "\
NAME          TYPE        CLUSTER-IP   EXTERNAL-IP   PORT(S)          AGE   SELECTOR
switchboard   ClusterIP   10.96.0.20   <none>        80/TCP,443/TCP   30m   app=informant,tier=front"
    );
}

#[test]
fn selectors_filter_and_empty_results_say_so() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl get pods -l app=witness -o name"), "pod/witness");
    assert_eq!(
        e.eval("kubectl get pods -l 'app in (witness,ghost)' -o name"),
        "pod/witness"
    );
    assert_eq!(e.eval("kubectl get pods -l app!=witness -o name"), "");
    assert_eq!(
        e.eval("kubectl get pods -l app=ghost"),
        "No resources found in default namespace."
    );
    assert_eq!(
        e.eval("kubectl get pods -l app=informant -n homicide -o name"),
        "pod/informant"
    );
    assert_eq!(
        e.eval("kubectl get svc -n homicide -l team=radio -o name"),
        "service/switchboard"
    );
    assert_eq!(e.eval("kubectl get nodes -l team=x"), "No resources found");
    assert_eq!(
        e.eval("kubectl get pods -l 'a b'"),
        "error: couldn't parse the selector string \"a b\": invalid selector"
    );
}

#[test]
fn all_namespaces_adds_a_column_and_sorts_by_namespace() {
    let mut e = engine();
    let out = e.eval("kubectl get pods -A");
    let mut lines = out.lines();
    let header = lines.next().unwrap();
    assert!(header.starts_with("NAMESPACE   NAME"), "{header}");
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split_whitespace().collect()).collect();
    assert_eq!(
        rows.iter().map(|r| (r[0], r[1])).collect::<Vec<_>>(),
        [("default", "witness"), ("homicide", "informant")]
    );
    assert_eq!(
        e.eval("kubectl get cm --all-namespaces -o name"),
        "configmap/empty\nconfigmap/dossier"
    );
    assert_eq!(e.eval("kubectl get pods -A -l app=ghost"), "No resources found");
    // Cluster-scoped kinds have no namespace column.
    assert!(e.eval("kubectl get nodes -A").starts_with("NAME "));
    assert_eq!(
        e.eval("kubectl get pod witness -A"),
        "error: a resource cannot be retrieved by name across all namespaces"
    );
}

#[test]
fn no_headers_sizes_columns_by_the_rows_alone() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl get cm --no-headers"), "empty   0   30m");
    assert_eq!(e.eval("kubectl get pods -A --no-headers").lines().count(), 2);
}

#[test]
fn jsonpath() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get pod witness -o jsonpath='{.metadata.name}'"),
        "witness"
    );
    assert_eq!(
        e.eval("kubectl get pods -o jsonpath='{.items[*].metadata.name}'"),
        "witness"
    );
    assert_eq!(
        e.eval(
            "kubectl get pods -A -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{\"\\n\"}{end}'"
        ),
        "default/witness\nhomicide/informant"
    );
    assert_eq!(
        e.eval(
            "kubectl get nodes -o jsonpath='{.items[?(@.metadata.name==\"worker-1\")].status.nodeInfo.kubeletVersion}'"
        ),
        "v1.36.2"
    );
    assert_eq!(
        e.eval("kubectl get cm dossier -n homicide -o jsonpath='{.data.suspect}' | base64"),
        "TW9yaWFydHk="
    );
    assert_eq!(e.eval("kubectl get pod witness -o jsonpath='{.nope}'"), "");
    assert_eq!(
        e.eval("kubectl get pods -o jsonpath"),
        "error: template format specified but no template given"
    );
    assert_eq!(
        e.eval("kubectl get pods -o jsonpath='{.items'"),
        "error: error parsing jsonpath {.items, unclosed action"
    );
    assert_eq!(
        e.eval("kubectl get pods -o go-template='{{.x}}'"),
        "error: unable to match a printer suitable for the output format \"go-template\", allowed formats are: json,jsonpath,name,wide,yaml"
    );
}

#[test]
fn other_formats_work_for_the_new_kinds() {
    let mut e = engine();
    assert!(e.eval("kubectl get cm empty -o yaml").contains("kind: ConfigMap"));
    assert!(e.eval("kubectl get svc -o json").contains("\"kind\": \"List\""));
}

#[test]
fn findings_can_target_any_kind() {
    let scenario = r#"{"schemaVersion":2,"id":"get","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["cluster.yaml"]},
        "findings":[{"id":"cm","clue":"the dossier","when":{"command":{"verb":"get","resource":"cm"}}}]}"#;
    let files = Files::from([("cluster.yaml".to_string(), CLUSTER.to_string())]);
    let mut e = Engine::from_scenario(scenario, &files, AppSet::bundled()).unwrap();
    let found = |e: &Engine| {
        serde_json::to_value(e.status()).unwrap()["findings"]["found"]
            .as_array()
            .unwrap()
            .len()
    };
    e.eval("kubectl get pods");
    assert_eq!(found(&e), 0);
    e.eval("kubectl get configmaps");
    assert_eq!(found(&e), 1);
}
