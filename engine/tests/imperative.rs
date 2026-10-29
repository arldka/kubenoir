//! End-to-end: the imperative commands (`run`, `create KIND`, `set image`, `scale`, `rollout
//! status|history|undo`, `label`, `expose`), which build or edit objects from flags.

use engine::{AppSet, Engine, Files};
use serde_json::Value;

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"imperative","files":["notes.txt","creds.txt"],
    "cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "findings":[
        {"id":"ns","clue":"you found a desk","when":{"command":{"verb":"create","resource":"namespace","name":"homicide"}}},
        {"id":"scaled","clue":"more officers","when":{"command":{"verb":"scale","resource":"deploy","name":"web"}}},
        {"id":"history","clue":"you read the log","when":{"command":{"verb":"rollout-history","name":"web"}}}]}"#;
const NODES: &str = "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n";

fn engine() -> Engine {
    let files = Files::from([
        ("nodes.yaml".to_string(), NODES.to_string()),
        ("notes.txt".to_string(), "the butler did it\n".to_string()),
        ("creds.txt".to_string(), "hunter2\n".to_string()),
    ]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn json(e: &mut Engine, what: &str) -> Value {
    serde_json::from_str(&e.eval(&format!("kubectl get {what} -o json"))).expect("json output")
}

fn found(e: &Engine) -> usize {
    serde_json::to_value(e.status()).unwrap()["findings"]["found"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn run_makes_a_pod() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl run informant --image=informant:1.0 --env=TIP_SOURCE=docks --env LOG=debug --port=8080"),
        "pod/informant created"
    );
    let pod = json(&mut e, "pod informant");
    assert_eq!(pod["metadata"]["labels"]["run"], "informant");
    let container = &pod["spec"]["containers"][0];
    assert_eq!(container["name"], "informant");
    assert_eq!(container["image"], "informant:1.0");
    assert_eq!(
        container["env"][0],
        serde_json::json!({"name": "TIP_SOURCE", "value": "docks"})
    );
    assert_eq!(container["ports"][0]["containerPort"], 8080);
    assert_eq!(pod["spec"]["restartPolicy"], "Always");

    e.advance(10);
    assert!(e.eval("kubectl get pod informant").contains("Running"));
}

#[test]
fn run_options_and_errors() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl run job-like --image=busybox --restart=Never --labels=team=radio,tier=back --command -- sleep 3600"),
        "pod/job-like created"
    );
    let pod = json(&mut e, "pod job-like");
    assert_eq!(pod["spec"]["restartPolicy"], "Never");
    assert_eq!(
        pod["metadata"]["labels"],
        serde_json::json!({"team": "radio", "tier": "back"})
    );
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!(["sleep", "3600"])
    );
    e.eval("kubectl run with-args --image=busybox -- echo hi");
    assert_eq!(
        json(&mut e, "pod with-args")["spec"]["containers"][0]["args"],
        serde_json::json!(["echo", "hi"])
    );

    assert_eq!(e.eval("kubectl run x"), "error: required flag(s) \"image\" not set");
    assert_eq!(e.eval("kubectl run --image=y"), "error: NAME is required for run");
    assert_eq!(e.eval("kubectl run a b --image=y"), "error: Unexpected args: [b]");
    assert_eq!(
        e.eval("kubectl run x --image=y --restart=Sometimes"),
        "error: invalid restart policy: Sometimes (one of Always, OnFailure, Never)"
    );
    assert_eq!(
        e.eval("kubectl run x --image=y --env=nope"),
        "error: invalid environment variable: nope"
    );
    assert_eq!(
        e.eval("kubectl run job-like --image=busybox"),
        "Error from server (AlreadyExists): pods \"job-like\" already exists"
    );
    assert_eq!(
        e.eval("kubectl run x --image=y -n nowhere"),
        "Error from server (NotFound): namespaces \"nowhere\" not found"
    );
    assert!(e
        .eval("kubectl run Bad_Name --image=y")
        .starts_with("The Pod \"Bad_Name\" is invalid: "));
    assert_eq!(
        e.eval("kubectl run x --image=y --dry-run=client"),
        "error: the flag --dry-run is not available in this terminal"
    );
}

#[test]
fn create_namespace_deployment_and_configmap() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl create namespace homicide"),
        "namespace/homicide created"
    );
    assert_eq!(
        e.eval("kubectl create ns homicide"),
        "Error from server (AlreadyExists): namespaces \"homicide\" already exists"
    );
    assert_eq!(
        e.eval("kubectl create deployment web --image=registry.example.com/team/web:2.0 --replicas=2 --port=8080 -n homicide"),
        "deployment.apps/web created"
    );
    let web = json(&mut e, "deployment web -n homicide");
    assert_eq!(web["spec"]["replicas"], 2);
    assert_eq!(web["spec"]["selector"]["matchLabels"]["app"], "web");
    assert_eq!(web["spec"]["template"]["spec"]["containers"][0]["name"], "web");
    assert_eq!(
        web["spec"]["template"]["spec"]["containers"][0]["ports"][0]["containerPort"],
        8080
    );
    e.advance(30);
    assert_eq!(e.eval("kubectl get pods -n homicide --no-headers").lines().count(), 2);

    assert_eq!(
        e.eval("kubectl create configmap dossier --from-literal=suspect=Moriarty --from-file=notes.txt --from-file=case=notes.txt -n homicide"),
        "configmap/dossier created"
    );
    let cm = json(&mut e, "cm dossier -n homicide");
    assert_eq!(
        cm["data"],
        serde_json::json!({"suspect": "Moriarty", "notes.txt": "the butler did it\n", "case": "the butler did it\n"})
    );
    assert_eq!(
        e.eval("kubectl create deployment web2"),
        "error: required flag(s) \"image\" not set"
    );
    assert_eq!(
        e.eval("kubectl create configmap"),
        "error: exactly NAME is required, got 0"
    );
    assert_eq!(
        e.eval("kubectl create configmap x --from-file=nope.txt"),
        "error: error reading nope.txt: no such file or directory"
    );
    assert_eq!(
        e.eval("kubectl create configmap x --from-literal=a=1 --from-literal=a=2"),
        "error: cannot add key \"a\", another key by that name already exists in Data for ConfigMap \"x\""
    );
    assert_eq!(
        e.eval("kubectl create namespace x --image=y"),
        "error: unknown flag: --image"
    );
    assert_eq!(
        e.eval("kubectl create namespace"),
        "error: exactly NAME is required, got 0"
    );
    assert!(e.eval("kubectl create namespace BAD").starts_with(
        "The Namespace \"BAD\" is invalid: metadata.name: Invalid value: \"BAD\": a lowercase RFC 1123 label"
    ));
}

#[test]
fn create_secret_encodes_values_exactly() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl create secret generic db --from-literal=password=pass --from-file=creds.txt"),
        "secret/db created"
    );
    // The file's trailing newline is part of the value; the literal has none.
    assert_eq!(
        e.eval("kubectl get secret db -o jsonpath='{.data.password}'"),
        "cGFzcw=="
    );
    assert_eq!(
        e.eval("kubectl get secret db -o jsonpath='{.data.creds\\.txt}' | base64 -d"),
        "hunter2"
    );
    assert_eq!(e.eval("kubectl get secret db -o jsonpath='{.type}'"), "Opaque");
    assert_eq!(
        e.eval("kubectl create secret generic typed --from-literal=k=v --type=example.com/custom"),
        "secret/typed created"
    );
    assert_eq!(
        e.eval("kubectl get secret typed -o jsonpath='{.type}'"),
        "example.com/custom"
    );

    assert_eq!(
        e.eval("kubectl create secret docker-registry pull --docker-server=registry.example.com --docker-username=u --docker-password=p --docker-email=u@example.com"),
        "secret/pull created"
    );
    assert_eq!(
        e.eval("kubectl get secret pull -o jsonpath='{.type}'"),
        "kubernetes.io/dockerconfigjson"
    );
    let config = e.eval("kubectl get secret pull -o jsonpath='{.data.\\.dockerconfigjson}' | base64 -d");
    let config: Value = serde_json::from_str(&config).expect("a docker config");
    assert_eq!(config["auths"]["registry.example.com"]["username"], "u");
    assert_eq!(config["auths"]["registry.example.com"]["auth"], "dTpw");
    assert_eq!(
        e.eval("kubectl create secret docker-registry bad --docker-username=u"),
        "error: required flag(s) \"docker-password\" not set"
    );
    assert_eq!(
        e.eval("kubectl create secret tls x"),
        "error: kubectl create secret tls is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl create secret"),
        "error: You must specify a subcommand for \"kubectl create secret\""
    );
    assert_eq!(
        e.eval("kubectl create secret generic db"),
        "Error from server (AlreadyExists): secrets \"db\" already exists"
    );
}

#[test]
fn set_image_scale_and_rollout() {
    let mut e = engine();
    e.eval("kubectl create deployment web --image=nginx:1.27 --replicas=2");
    e.advance(30);
    assert_eq!(
        e.eval("kubectl rollout status deployment/web"),
        "deployment \"web\" successfully rolled out"
    );
    assert_eq!(
        e.eval("kubectl rollout history deployment/web"),
        "deployment.apps/web \nREVISION  CHANGE-CAUSE\n1         <none>\n"
    );

    assert_eq!(
        e.eval("kubectl set image deployment/web nginx=nginx:1.28"),
        "deployment.apps/web image updated"
    );
    assert_eq!(json(&mut e, "deployment web")["metadata"]["generation"], 2);
    let status = e.eval("kubectl rollout status deployment/web");
    assert!(
        status.starts_with("Waiting for deployment \"web\" rollout to finish: "),
        "{status}"
    );
    e.advance(60);
    assert_eq!(
        e.eval("kubectl rollout status deploy web"),
        "deployment \"web\" successfully rolled out"
    );
    assert!(e
        .eval("kubectl rollout history deployment/web")
        .contains("\n2         <none>"));

    // Back to the first revision.
    assert_eq!(
        e.eval("kubectl rollout undo deployment/web"),
        "deployment.apps/web rolled back"
    );
    assert_eq!(
        e.eval("kubectl get deployment web -o jsonpath='{.spec.template.spec.containers[0].image}'"),
        "nginx:1.27"
    );
    e.advance(60);
    assert_eq!(
        e.eval("kubectl rollout undo deployment/web --to-revision=2"),
        "deployment.apps/web rolled back"
    );
    assert_eq!(
        e.eval("kubectl get deployment web -o jsonpath='{.spec.template.spec.containers[0].image}'"),
        "nginx:1.28"
    );
    assert_eq!(
        e.eval("kubectl rollout undo deployment/web --to-revision=9"),
        "error: unable to find specified revision 9 in history"
    );
    e.advance(60);

    assert_eq!(
        e.eval("kubectl scale deployment/web --replicas=4"),
        "deployment.apps/web scaled"
    );
    e.advance(60);
    assert_eq!(e.eval("kubectl get pods --no-headers | grep -c Running"), "4");
    assert_eq!(
        e.eval("kubectl scale deploy web --replicas=0"),
        "deployment.apps/web scaled"
    );
    e.advance(60);
    assert_eq!(e.eval("kubectl get pods"), "No resources found in default namespace.");
}

#[test]
fn set_image_scale_and_rollout_errors() {
    let mut e = engine();
    e.eval("kubectl create deployment web --image=nginx:1.27");
    // One revision only: nothing to go back to.
    e.advance(5);
    assert_eq!(
        e.eval("kubectl rollout undo deployment/web"),
        "error: unable to find last revision"
    );
    assert_eq!(
        e.eval("kubectl set image deployment/web nope=x"),
        "error: unable to find container named \"nope\""
    );
    assert_eq!(
        e.eval("kubectl set image deployment/ghost web=x"),
        "Error from server (NotFound): deployments.apps \"ghost\" not found"
    );
    assert_eq!(
        e.eval("kubectl set image deployment/web"),
        "error: at least one image update is required"
    );
    assert_eq!(
        e.eval("kubectl set image deployment/web *=nginx:1.29"),
        "deployment.apps/web image updated"
    );
    assert_eq!(
        e.eval("kubectl set image cm/web a=b"),
        "error: kubectl set image for configmaps is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl scale deployment/web"),
        "error: required flag(s) \"replicas\" not set"
    );
    assert_eq!(
        e.eval("kubectl scale deployment/web --replicas=-1"),
        "The Deployment \"web\" is invalid: spec.replicas: Invalid value: -1: must be greater than or equal to 0"
    );
    assert_eq!(
        e.eval("kubectl scale deployment/web --replicas=x"),
        "error: invalid argument \"x\" for \"--replicas\" flag: strconv.ParseInt: parsing \"x\": invalid syntax"
    );
    assert_eq!(
        e.eval("kubectl scale pod/x --replicas=2"),
        "error: kubectl scale for pods is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl rollout status deployment/ghost"),
        "Error from server (NotFound): deployments.apps \"ghost\" not found"
    );
    assert_eq!(
        e.eval("kubectl rollout status pod/x"),
        "error: kubectl rollout status for pods is not available in this terminal"
    );
    assert_eq!(e.eval("kubectl rollout history"), "error: You must specify the type of resource to act on. Use \"kubectl api-resources\" for a complete list of supported resources.");
}

#[test]
fn set_image_on_a_running_pod_is_allowed_and_the_rest_of_its_spec_is_not() {
    let mut e = engine();
    e.eval("kubectl run informant --image=informant:1.0");
    assert_eq!(
        e.eval("kubectl set image pod/informant informant=informant:2.0"),
        "pod/informant image updated"
    );
    assert_eq!(
        e.eval("kubectl get pod informant -o jsonpath='{.spec.containers[0].image}'"),
        "informant:2.0"
    );
}

#[test]
fn label_adds_changes_and_removes() {
    let mut e = engine();
    e.eval("kubectl run informant --image=informant:1.0");
    e.eval("kubectl run witness --image=informant:1.0");
    assert_eq!(
        e.eval("kubectl label pod informant team=radio tier=front"),
        "pod/informant labeled"
    );
    assert_eq!(e.eval("kubectl get pods -l team=radio -o name"), "pod/informant");
    assert_eq!(
        e.eval("kubectl label pod informant team=cid"),
        "error: 'team' already has a value (radio), and --overwrite is false"
    );
    assert_eq!(
        e.eval("kubectl label pod informant team=cid --overwrite"),
        "pod/informant labeled"
    );
    assert_eq!(
        e.eval("kubectl label pod informant team=cid --overwrite"),
        "pod/informant not labeled"
    );
    assert_eq!(e.eval("kubectl label pod informant tier-"), "pod/informant labeled");
    assert_eq!(
        e.eval("kubectl get pod informant -o jsonpath='{.metadata.labels}'"),
        "{\"run\":\"informant\",\"team\":\"cid\"}"
    );
    assert_eq!(
        e.eval("kubectl label pods informant witness status=seen"),
        "pod/informant labeled\npod/witness labeled"
    );
    assert_eq!(
        e.eval("kubectl label pod ghost a=b"),
        "Error from server (NotFound): pods \"ghost\" not found"
    );
    assert_eq!(
        e.eval("kubectl label pod informant"),
        "error: at least one label update is required"
    );
    assert_eq!(
        e.eval("kubectl label pod informant =x"),
        "error: invalid label spec: =x"
    );
    // Any kind can be labelled, cluster-scoped ones included.
    assert_eq!(e.eval("kubectl label node worker-1 disk=ssd"), "node/worker-1 labeled");
    assert_eq!(e.eval("kubectl get nodes -l disk=ssd -o name"), "node/worker-1");
    e.eval("kubectl create namespace homicide");
    assert_eq!(
        e.eval("kubectl label ns homicide case=open"),
        "namespace/homicide labeled"
    );
    // The pod's spec is untouched by labelling.
    e.advance(10);
    assert!(e.eval("kubectl get pod informant").contains("Running"));
}

#[test]
fn expose_makes_a_service() {
    let mut e = engine();
    e.eval("kubectl create deployment web --image=nginx:1.27 --port=8080");
    assert_eq!(e.eval("kubectl expose deployment web"), "service/web exposed");
    let svc = json(&mut e, "service web");
    assert_eq!(svc["spec"]["selector"], serde_json::json!({"app": "web"}));
    assert_eq!(svc["spec"]["ports"][0]["port"], 8080);
    assert_eq!(svc["spec"]["ports"][0]["targetPort"], 8080);
    assert_eq!(svc["spec"]["type"], "ClusterIP");
    assert_eq!(svc["metadata"]["labels"]["app"], "web");

    assert_eq!(
        e.eval(
            "kubectl expose deployment web --port=80 --target-port=http --name=front --type=NodePort --protocol=TCP"
        ),
        "service/front exposed"
    );
    let front = json(&mut e, "service front");
    assert_eq!(
        (
            front["spec"]["ports"][0]["port"].as_i64(),
            front["spec"]["ports"][0]["targetPort"].as_str()
        ),
        (Some(80), Some("http"))
    );
    assert_eq!(front["spec"]["type"], "NodePort");

    e.eval("kubectl run informant --image=informant:1.0 --port=9000");
    assert_eq!(
        e.eval("kubectl expose pod informant --name=switchboard"),
        "service/switchboard exposed"
    );
    assert_eq!(
        e.eval("kubectl get svc switchboard -o jsonpath='{.spec.selector.run}'"),
        "informant"
    );

    assert_eq!(
        e.eval("kubectl expose deployment web"),
        "Error from server (AlreadyExists): services \"web\" already exists"
    );
    e.eval("kubectl create deployment bare --image=busybox");
    assert_eq!(
        e.eval("kubectl expose deployment bare"),
        "error: couldn't find port via --port flag or introspection"
    );
    assert_eq!(
        e.eval("kubectl expose deployment ghost --port=80"),
        "Error from server (NotFound): deployments.apps \"ghost\" not found"
    );
    assert_eq!(
        e.eval("kubectl expose deployment web --type=Sideways --port=80"),
        "error: unknown service type \"Sideways\" (one of ClusterIP, NodePort, LoadBalancer)"
    );
    assert_eq!(
        e.eval("kubectl expose cm x"),
        "error: kubectl expose for configmaps is not available in this terminal"
    );
    e.eval("kubectl run unlabelled --image=x --labels=a=b");
    e.eval("kubectl label pod unlabelled a-");
    assert_eq!(
        e.eval("kubectl expose pod unlabelled --port=80"),
        "error: couldn't retrieve selectors via --selector flag or introspection: the pod has no labels and cannot be exposed"
    );
}

#[test]
fn commands_are_findings_by_kind_and_name_but_failures_are_not() {
    let mut e = engine();
    e.eval("kubectl create namespace other");
    assert_eq!(found(&e), 0, "a different name does not match");
    e.eval("kubectl create deployment web --image=nginx:1.27");
    e.eval("kubectl scale deployment ghost --replicas=2");
    assert_eq!(found(&e), 0, "a failed command is no finding");
    e.eval("kubectl scale deployment web --replicas=2");
    assert_eq!(found(&e), 1);
    e.eval("kubectl rollout history deployment/web");
    assert_eq!(found(&e), 2);
    e.eval("kubectl create ns homicide");
    assert_eq!(found(&e), 3);
}

#[test]
fn the_commands_have_help_that_matches_them() {
    let mut e = engine();
    for command in [
        "run",
        "scale",
        "label",
        "expose",
        "set image",
        "rollout status",
        "rollout history",
        "rollout undo",
        "create namespace",
        "create deployment",
        "create configmap",
        "create secret generic",
        "create secret docker-registry",
    ] {
        let page = e.eval(&format!("kubectl {command} --help"));
        assert!(
            page.contains(&format!("Usage:\n  kubectl {command}")),
            "{command}\n{page}"
        );
    }
    assert!(e
        .eval("kubectl create --help")
        .contains("Available Commands:\n  configmap Create a config map"));
    assert!(
        e.eval("kubectl create secret --help").contains("  docker-registry  ")
            || e.eval("kubectl create secret --help").contains("docker-registry")
    );
    assert_eq!(
        e.eval("kubectl create ns --help"),
        e.eval("kubectl create namespace --help")
    );
    assert_eq!(
        e.eval("kubectl help create secret generic"),
        e.eval("kubectl create secret generic --help")
    );
    let overview = e.eval("kubectl --help");
    for command in ["run", "expose", "scale", "label"] {
        assert!(overview.contains(&format!("  {command} ")), "{command}");
    }
}
