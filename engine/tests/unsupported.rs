//! SPEC V5: whatever the terminal cannot do fails loudly, names the thing, and changes nothing.
//! Real kubectl has all of these; the message must say "not available here" rather than suggest
//! a typo, and anything genuinely unknown must still read as unknown.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"unsupported",
    "files":["a.yaml","b.yaml","d/c.yaml","d/e.yml","d/notes.txt","d/sub/f.yaml","dot/.hidden.yaml","jobs.yaml"],
    "cluster":{"now":"2026-10-27T18:30:00Z"}}"#;

fn cm(name: &str) -> String {
    format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}\n")
}

fn engine() -> Engine {
    let files = Files::from([
        ("a.yaml".to_string(), cm("a")),
        ("b.yaml".to_string(), cm("b")),
        ("d/c.yaml".to_string(), cm("c")),
        ("d/e.yml".to_string(), cm("e")),
        ("d/notes.txt".to_string(), "not a manifest\n".to_string()),
        ("d/sub/f.yaml".to_string(), cm("f")),
        ("dot/.hidden.yaml".to_string(), cm("hidden")),
        (
            "jobs.yaml".to_string(),
            "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: nightly\n".to_string(),
        ),
    ]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn names(e: &mut Engine) -> String {
    e.eval("kubectl get cm -o name").replace('\n', " ")
}

#[test]
fn kubectl_commands_that_exist_but_are_not_offered_say_so() {
    let mut e = engine();
    for verb in [
        "annotate",
        "cordon",
        "drain",
        "top",
        "version",
        "config",
        "port-forward",
        "diff",
    ] {
        assert_eq!(
            e.eval(&format!("kubectl {verb} x")),
            format!("error: kubectl {verb} is not available in this terminal"),
            "{verb}"
        );
    }
    // The boss answers `kubectl edit`, however it is asked, and nothing is changed.
    for line in [
        "kubectl edit pod x",
        "kubectl edit",
        "kubectl edit --help",
        "kubectl -n x edit deploy/y",
        "kubectl edit pod x -o yaml",
    ] {
        assert_eq!(
            e.eval(line),
            "We don't do that here, we are not that kind of people.",
            "{line}"
        );
    }
    assert_eq!(
        e.eval("kubectl edit pod x | cat"),
        "We don't do that here, we are not that kind of people."
    );
    assert_eq!(
        e.eval("kubectl edit pod x | grep -c ."),
        "We don't do that here, we are not that kind of people.\n0"
    );
    // The help page of one says the same, not "unknown command".
    assert_eq!(
        e.eval("kubectl top --help"),
        "error: kubectl top is not available in this terminal"
    );
    // Genuinely unknown words are still unknown.
    assert_eq!(
        e.eval("kubectl bogus"),
        "error: unknown command \"bogus\" for \"kubectl\""
    );
    assert_eq!(e.eval("bogus"), "bogus: command not found");
}

#[test]
fn subcommands_that_exist_but_are_not_offered_say_so() {
    let mut e = engine();
    for (line, named) in [
        ("kubectl rollout pause deploy/x", "kubectl rollout pause"),
        ("kubectl rollout resume deploy/x", "kubectl rollout resume"),
        ("kubectl set selector deploy/x a=b", "kubectl set selector"),
        ("kubectl set resources deploy/x --limits=cpu=1", "kubectl set resources"),
        ("kubectl create service clusterip x", "kubectl create service"),
        ("kubectl create job x --image=y", "kubectl create job"),
        ("kubectl create secret tls x", "kubectl create secret tls"),
    ] {
        assert_eq!(
            e.eval(line),
            format!("error: {named} is not available in this terminal"),
            "{line}"
        );
    }
    assert_eq!(
        e.eval("kubectl rollout nope deploy/x"),
        "error: unknown command \"nope\" for \"kubectl rollout\""
    );
}

#[test]
fn flags_that_exist_but_are_not_offered_say_so_and_typos_do_not() {
    let mut e = engine();
    for (line, flag) in [
        ("kubectl get pods --show-labels", "--show-labels"),
        ("kubectl get pods -w", "-w"),
        ("kubectl get pods --watch", "--watch"),
        (
            "kubectl get pods --field-selector=status.phase=Running",
            "--field-selector",
        ),
        ("kubectl get pods --sort-by=.metadata.name", "--sort-by"),
        ("kubectl logs web -f", "-f"),
        ("kubectl logs web --since=1h", "--since"),
        ("kubectl delete pod --all", "--all"),
        ("kubectl delete pod x -l a=b", "-l"),
        ("kubectl apply -f a.yaml --dry-run=client", "--dry-run"),
        ("kubectl apply -f a.yaml --server-side", "--server-side"),
        ("kubectl apply -f a.yaml -o name", "-o"),
        ("kubectl get pods -v=6", "-v"),
        ("kubectl get pods --kubeconfig x", "--kubeconfig"),
        ("kubectl --context prod get pods", "--context"),
        ("kubectl get pods -R", "-R"),
    ] {
        assert_eq!(
            e.eval(line),
            format!("error: the flag {flag} is not available in this terminal"),
            "{line}"
        );
    }
    assert_eq!(e.eval("kubectl get pods --bogus"), "error: unknown flag: --bogus");
    assert_eq!(
        e.eval("kubectl get pods -z"),
        "error: unknown shorthand flag: 'z' in -z"
    );
}

#[test]
fn resource_types_that_exist_but_are_not_simulated_say_so() {
    let mut e = engine();
    for arg in [
        "jobs",
        "cronjob",
        "pv",
        "pvc",
        "sa",
        "ingress",
        "ds",
        "sts",
        "netpol",
        "hpa",
        "all",
        "serviceaccounts",
    ] {
        assert_eq!(
            e.eval(&format!("kubectl get {arg}")),
            format!("error: the resource type {arg} is not available in this terminal"),
            "{arg}"
        );
    }
    assert_eq!(
        e.eval("kubectl get pods,svc"),
        "error: listing several resource types at once (pods,svc) is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl get widgets"),
        "error: the server doesn't have a resource type \"widgets\""
    );
    assert_eq!(
        e.eval("kubectl explain jobs"),
        "error: the resource type jobs is not available in this terminal"
    );
}

#[test]
fn commands_that_only_handle_some_kinds_name_the_kind_they_refuse() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl describe cronjob x"),
        "error: the resource type cronjob is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl describe cm x"),
        "error: kubectl describe for configmaps is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl describe node x"),
        "error: kubectl describe for nodes is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl logs nodes/x"),
        "error: kubectl logs for nodes is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl set env cm/x A=b"),
        "error: kubectl set env for configmaps is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl rollout restart svc/x"),
        "error: kubectl rollout restart for services is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl describe jobs x"),
        "error: the resource type jobs is not available in this terminal"
    );
}

#[test]
fn manifests_of_kinds_that_are_not_simulated_are_refused_by_name() {
    let mut e = engine();
    for command in ["apply", "create", "replace"] {
        assert_eq!(
            e.eval(&format!("kubectl {command} -f jobs.yaml")),
            "error: the Job kind is not available in this terminal (in \"jobs.yaml\")",
            "{command}"
        );
    }
    assert_eq!(
        e.eval("kubectl delete -f jobs.yaml"),
        "error: the Job kind is not available in this terminal (in \"jobs.yaml\")"
    );
    e.write_file(
        "crd.yaml",
        "apiVersion: example.com/v1\nkind: Widget\nmetadata:\n  name: w\n",
    );
    assert!(e
        .eval("kubectl apply -f crd.yaml")
        .contains("ensure CRDs are installed first"));
    // Nothing was created by the refusals.
    assert_eq!(e.eval("kubectl get cm"), "No resources found in default namespace.");
}

#[test]
fn global_namespace_flag_may_come_before_the_verb() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl apply -f a.yaml"), "configmap/a created");
    e.eval("kubectl create -f a.yaml -n default");
    e.write_file(
        "ns.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: homicide\n",
    );
    e.eval("kubectl apply -f ns.yaml");
    assert_eq!(e.eval("kubectl -n homicide apply -f b.yaml"), "configmap/b created");
    assert_eq!(e.eval("kubectl --namespace=homicide get cm -o name"), "configmap/b");
    assert_eq!(e.eval("kubectl -nhomicide get cm -o name"), "configmap/b");
    assert_eq!(
        e.eval("kubectl -n homicide get cm -n default -o name"),
        "configmap/a",
        "the later flag wins"
    );
    assert_eq!(
        e.eval("kubectl -n homicide explain pod").lines().next().unwrap(),
        "KIND:       Pod"
    );
    assert_eq!(e.eval("kubectl -n"), "error: flag needs an argument: -n, --namespace");
    assert_eq!(
        e.eval("kubectl -n homicide"),
        "error: You must specify the type of command"
    );
}

#[test]
fn wildcards_expand_against_the_terminals_files() {
    let mut e = engine();
    assert_eq!(e.eval("ls *.yaml"), "a.yaml\nb.yaml\njobs.yaml");
    assert_eq!(e.eval("ls d/*.y*"), "d/c.yaml\nd/e.yml");
    assert_eq!(e.eval("ls ?.yaml"), "a.yaml\nb.yaml");
    assert_eq!(e.eval("ls [ab].yaml"), "a.yaml\nb.yaml");
    assert_eq!(e.eval("ls [!a].yaml"), "b.yaml");
    // A single matched directory is listed by its contents, as `ls` does.
    assert_eq!(e.eval("ls d/s*"), "f.yaml");
    assert_eq!(e.eval("cat ?.yaml | grep -c kind"), "2");
    // Quoted, escaped or unmatched wildcards stay as typed.
    assert_eq!(
        e.eval("ls '*.yaml'"),
        "ls: cannot access '*.yaml': No such file or directory"
    );
    assert_eq!(
        e.eval("ls \\*.yaml"),
        "ls: cannot access '*.yaml': No such file or directory"
    );
    assert_eq!(
        e.eval("ls *.nothing"),
        "ls: cannot access '*.nothing': No such file or directory"
    );
    // A leading dot must be typed.
    assert_eq!(
        e.eval("ls dot/*"),
        "ls: cannot access 'dot/*': No such file or directory"
    );
    assert_eq!(e.eval("ls dot/.h*"), "dot/.hidden.yaml");
    // The expansion happens before the command sees it.
    assert_eq!(e.eval("kubectl apply -f ?.yaml"), "error: Unexpected args: [b.yaml]");
    assert_eq!(e.eval("echo *.yaml"), "a.yaml b.yaml jobs.yaml");
    assert_eq!(e.eval("echo '*.yaml'"), "*.yaml");
    assert_eq!(e.eval("kubectl get pods -o jsonpath={.items[*].metadata.name}"), "");
}

#[test]
fn a_directory_given_to_f_applies_the_manifests_directly_inside_it() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl apply -f d"), "configmap/c created\nconfigmap/e created");
    assert_eq!(
        names(&mut e),
        "configmap/c configmap/e",
        "notes.txt and the subdirectory are skipped"
    );
    assert_eq!(
        e.eval("kubectl apply -f d/"),
        "configmap/c unchanged\nconfigmap/e unchanged"
    );
    assert_eq!(
        e.eval("kubectl delete -f d"),
        "configmap \"c\" deleted\nconfigmap \"e\" deleted"
    );
    assert_eq!(e.eval("kubectl apply -f d/sub"), "configmap/f created");
    assert_eq!(
        e.eval("kubectl apply -f nope"),
        "error: the path \"nope\" does not exist"
    );
    e.write_file("empty/readme.txt", "x");
    assert_eq!(e.eval("kubectl apply -f empty"), "error: no objects passed to apply");
}

#[test]
fn redirects_that_would_misfire_are_refused() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get pods 2> err.txt"),
        "error: redirecting a file descriptor (such as 2>) is not available in this terminal"
    );
    assert_eq!(
        e.eval("kubectl get pods 2>&1"),
        "error: redirecting a file descriptor (such as 2>) is not available in this terminal"
    );
    assert_eq!(
        e.eval("echo hi >&2"),
        "error: redirecting to a file descriptor (>&) is not available in this terminal"
    );
    // A number that is just an argument is fine.
    assert_eq!(e.eval("echo 2 > two.txt"), "");
    assert_eq!(e.eval("cat two.txt"), "2");
    assert_eq!(e.eval("echo hi > /dev/null"), "");
    assert_eq!(e.eval("ls"), "a.yaml\nb.yaml\nd\ndot\njobs.yaml\ntwo.txt");
    for target in ["/tmp/x", "~/x", "../x", "d/../../x"] {
        assert_eq!(
            e.eval(&format!("echo hi > {target}")),
            format!("error: {target}: writing outside the terminal's own files is not available; use a relative path"),
            "{target}"
        );
    }
    assert_eq!(e.eval("echo hi > d"), "error: d: Is a directory");
}

#[test]
fn nothing_unsupported_leaves_a_trace_in_the_cluster() {
    let mut e = engine();
    let before = e.eval("kubectl get all");
    for line in [
        "kubectl exec x",
        "kubectl get pods -w",
        "kubectl apply -f a.yaml --dry-run=client",
        "kubectl apply -f jobs.yaml",
        "kubectl delete pod --all",
        "kubectl rollout pause deploy/x",
        "kubectl create job x --image=y",
        "kubectl apply -f ?.yaml",
    ] {
        let out = e.eval(line);
        assert!(out.starts_with("error:"), "{line} => {out}");
    }
    assert_eq!(e.eval("kubectl get cm"), "No resources found in default namespace.");
    assert_eq!(e.eval("kubectl get ns -o name").lines().count(), 4);
    assert_eq!(before, "error: the resource type all is not available in this terminal");
}
