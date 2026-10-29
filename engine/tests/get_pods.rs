//! End-to-end: sample scenario file -> Engine -> kubectl output.

mod common;

use engine::{AppSet, Engine, Files};

fn engine() -> Engine {
    common::engine("sample-crashloop")
}

#[test]
fn get_pods_default_namespace() {
    assert_eq!(
        engine().eval("kubectl get pods"),
        "\
NAME                           READY   STATUS             RESTARTS   AGE
payments-api-5c7d4f9b8-9hjv2   0/1     CrashLoopBackOff   14         47m
web-7d9f8b6c5-m8q7z            1/1     Running            0          3d
web-7d9f8b6c5-x2k4p            1/1     Running            0          3d"
    );
}

#[test]
fn get_pods_kube_system() {
    assert_eq!(
        engine().eval("kubectl get po -n kube-system"),
        "\
NAME                      READY   STATUS    RESTARTS   AGE
coredns-6d8c4cb4d-5fwtn   1/1     Running   1          12d
etcd-control-plane        1/1     Running   0          12d"
    );
}

#[test]
fn get_pods_wide_includes_ip_and_node() {
    let out = engine().eval("kubectl get pods -o wide");
    let crash = out.lines().find(|l| l.starts_with("payments-api")).unwrap();
    assert!(crash.contains("10.244.2.9"), "{crash}");
    assert!(crash.contains("worker-2"), "{crash}");
}

#[test]
fn get_pod_by_name_json_and_yaml() {
    let mut e = engine();
    let json: serde_json::Value =
        serde_json::from_str(&e.eval("kubectl get pod payments-api-5c7d4f9b8-9hjv2 -o json")).unwrap();
    assert_eq!(json["kind"], "Pod");
    assert_eq!(json["status"]["containerStatuses"][0]["restartCount"], 14);

    let yaml = e.eval("kubectl get pod payments-api-5c7d4f9b8-9hjv2 -o yaml");
    assert!(yaml.contains("reason: CrashLoopBackOff"), "{yaml}");
}

#[test]
fn get_pods_name_format() {
    assert_eq!(
        engine().eval("kubectl get pods -o name"),
        "pod/payments-api-5c7d4f9b8-9hjv2\npod/web-7d9f8b6c5-m8q7z\npod/web-7d9f8b6c5-x2k4p"
    );
}

#[test]
fn errors_read_like_kubectl() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get pod nope"),
        "Error from server (NotFound): pods \"nope\" not found"
    );
    assert_eq!(
        e.eval("kubectl get pods -n ghost"),
        "No resources found in ghost namespace."
    );
    assert_eq!(
        e.eval("kubectl get widgets"),
        "error: the server doesn't have a resource type \"widgets\""
    );
    assert_eq!(
        e.eval("kubectl apply -f x.yaml"),
        "error: the path \"x.yaml\" does not exist"
    );
    assert_eq!(
        e.eval("kubectl port-forward web 8080"),
        "error: unknown command \"port-forward\" for \"kubectl\""
    );
}

#[test]
fn invalid_scenario_is_rejected() {
    assert!(Engine::from_scenario("{}", &Files::new(), AppSet::bundled()).is_err());
}
