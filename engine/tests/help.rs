//! End-to-end: `--help` for every command, and `kubectl explain`.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"help","cluster":{"now":"2026-10-27T18:30:00Z"},
    "findings":[{"id":"docs","clue":"you read the manual","when":{"command":{"verb":"explain","name":"pod.spec*"}}}]}"#;

fn engine() -> Engine {
    Engine::from_scenario(SCENARIO, &Files::new(), AppSet::bundled()).expect("scenario loads")
}

fn found(e: &Engine) -> usize {
    serde_json::to_value(e.status()).unwrap()["findings"]["found"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn the_overview_lists_every_command_that_works() {
    let mut e = engine();
    let out = e.eval("kubectl --help");
    assert!(
        out.starts_with("kubectl controls the Kubernetes cluster manager"),
        "{out}"
    );
    for command in [
        "create", "explain", "get", "delete", "set", "rollout", "describe", "logs", "apply", "patch", "replace",
    ] {
        assert!(out.contains(&format!("  {command} ")), "{command}\n{out}");
    }
    assert_eq!(out, e.eval("kubectl help"));
    assert_eq!(out, e.eval("kubectl -h"));
    assert_eq!(e.eval("kubectl help get"), e.eval("kubectl get --help"));
    assert_eq!(e.eval("kubectl help nope"), "Unknown help topic [`nope`]");
    assert_eq!(e.eval("kubectl help set env"), e.eval("kubectl set env --help"));
}

#[test]
fn every_command_has_a_page_and_unknown_ones_still_fail() {
    let mut e = engine();
    for command in [
        "get",
        "describe",
        "logs",
        "set",
        "set env",
        "rollout",
        "rollout restart",
        "delete",
        "apply",
        "create",
        "replace",
        "patch",
        "explain",
    ] {
        let page = e.eval(&format!("kubectl {command} --help"));
        assert!(page.contains("Usage:\n  kubectl "), "{command}\n{page}");
    }
    assert_eq!(
        e.eval("kubectl port-forward --help"),
        "error: unknown command \"port-forward\" for \"kubectl\""
    );
    // Help works whatever else was typed.
    assert_eq!(
        e.eval("kubectl get pods -o nonsense --help"),
        e.eval("kubectl get --help")
    );
    assert!(e.eval("kubectl get --help").contains("--selector='':"));
    assert!(
        e.eval("kubectl get --help | grep -c jsonpath").starts_with('1')
            || e.eval("kubectl get --help | grep -c jsonpath") != "0"
    );
}

#[test]
fn shell_commands_have_help_too() {
    let mut e = engine();
    assert!(e
        .eval("grep --help")
        .starts_with("Usage: grep [OPTION]... PATTERNS [FILE]..."));
    assert!(e.eval("base64 --help").contains("-d, --decode"));
    assert!(e.eval("head --help").contains("-n, --lines=VALUE"));
    assert!(e.eval("cat --help").starts_with("Usage: cat FILE..."));
    assert!(e.eval("ls --help").starts_with("Usage: ls [FILE]..."));
    // `echo --help` prints its argument, like the shell's own echo.
    assert_eq!(e.eval("echo --help"), "--help");
    assert_eq!(e.eval("kubectl --help | grep -c 'Basic Commands'"), "1");
}

#[test]
fn explain_documents_resources_and_fields() {
    let mut e = engine();
    let pod = e.eval("kubectl explain pod");
    assert!(
        pod.starts_with("KIND:       Pod\nVERSION:    v1\n\nDESCRIPTION:\n"),
        "{pod}"
    );
    let field = e.eval("kubectl explain deployment.spec.template.spec.containers.image");
    assert!(field.starts_with("GROUP:      apps\nKIND:       Deployment\nVERSION:    v1\n\nFIELD: image <string>\n\nDESCRIPTION:\n    Container image name."), "{field}");
    assert_eq!(
        e.eval("kubectl explain pods.spec.nope"),
        "error: field \"nope\" does not exist"
    );
    assert_eq!(
        e.eval("kubectl explain widgets"),
        "error: the server doesn't have a resource type \"widgets\""
    );
    assert_eq!(
        e.eval("kubectl explain"),
        "error: You must specify the type of resource to explain. Use \"kubectl api-resources\" for a complete list of supported resources."
    );
    assert_eq!(
        e.eval("kubectl explain pod --recursive"),
        "error: unknown flag: --recursive"
    );
    assert_eq!(e.eval("kubectl explain pod svc"), "error: Unexpected args: [svc]");
    assert!(
        e.eval("kubectl explain pod.spec.containers | grep -c required")
            .parse::<i32>()
            .unwrap()
            >= 1
    );
}

#[test]
fn explain_can_be_a_finding_but_a_failed_one_is_not() {
    let mut e = engine();
    e.eval("kubectl explain pod.spec.nope");
    e.eval("kubectl explain svc.spec");
    assert_eq!(found(&e), 0);
    e.eval("kubectl explain pod.spec.containers");
    assert_eq!(found(&e), 1);
}

#[test]
fn completion_offers_the_new_commands_and_documented_flags() {
    let e = engine();
    let verbs = e.complete("kubectl ").candidates;
    for v in ["apply", "explain", "patch", "replace", "create"] {
        assert!(verbs.contains(&v.to_string()), "{v}");
    }
    let flags = e.complete("kubectl get pods --").candidates;
    for f in [
        "--selector",
        "--all-namespaces",
        "--no-headers",
        "--output",
        "--namespace",
    ] {
        assert!(flags.contains(&f.to_string()), "{f}");
    }
    // The value of -l is free text, not a pod name.
    assert!(e.complete("kubectl get pods -l ").candidates.is_empty());
}
