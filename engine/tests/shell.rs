//! End-to-end: pipelines and redirection in the terminal's shell.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"shell",
    "files":["notes.txt"],
    "cluster":{"now":"2026-10-27T18:30:00Z","manifests":["pods.yaml"]},
    "findings":[{"id":"crash","clue":"you spotted the crash",
        "when":{"command":{"verb":"get","resource":"pods"}}}]}"#;

const PODS: &str = "\
apiVersion: v1
kind: Node
metadata:
  name: worker-1
---
apiVersion: v1
kind: Pod
metadata:
  name: informant
spec:
  containers:
  - name: informant
    image: informant:1.0
---
apiVersion: v1
kind: Pod
metadata:
  name: witness
spec:
  containers:
  - name: witness
    image: informant:1.0
";

fn engine() -> Engine {
    let files = Files::from([
        ("pods.yaml".to_string(), PODS.to_string()),
        ("notes.txt".to_string(), "line one\nline two\nline three\n".to_string()),
    ]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn found(e: &Engine) -> usize {
    serde_json::to_value(e.status()).unwrap()["findings"]["found"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn the_trailing_newline_changes_what_base64_encodes() {
    let mut e = engine();
    assert_eq!(e.eval("echo pass | base64"), "cGFzcwo=");
    assert_eq!(e.eval("echo -n pass | base64"), "cGFzcw==");
    assert_eq!(e.eval("echo -n pass | base64 | base64 -d"), "pass");
    assert_eq!(e.eval("echo cGFzcw== | base64 -d"), "pass");
    assert_eq!(e.eval("printf pass"), "printf: command not found");
}

#[test]
fn kubectl_and_files_feed_filters() {
    let mut e = engine();
    e.advance(10);
    let pods = e.eval("kubectl get pods | grep inform");
    assert!(pods.starts_with("informant") && pods.contains("Running"), "{pods}");
    assert_eq!(pods.lines().count(), 1);
    assert_eq!(
        e.eval("kubectl get pods | head -n 1"),
        e.eval("kubectl get pods").lines().next().unwrap()
    );
    assert_eq!(e.eval("kubectl get pods | grep -c Running"), "2");
    assert_eq!(e.eval("cat notes.txt | grep -n two"), "2:line two");
    assert_eq!(e.eval("cat notes.txt | head -2 | grep -v one"), "line two");
    assert_eq!(e.eval("grep three notes.txt"), "line three");
    assert_eq!(e.eval("head -n 1 notes.txt"), "line one");
    assert_eq!(e.eval("ls | grep notes"), "notes.txt");
}

#[test]
fn errors_go_to_the_terminal_not_down_the_pipe() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl get pod nope | grep -c ."),
        "Error from server (NotFound): pods \"nope\" not found\n0"
    );
    assert_eq!(
        e.eval("cat missing.txt | base64"),
        "cat: missing.txt: No such file or directory"
    );
    assert_eq!(
        e.eval("kubectl nope | cat"),
        "error: unknown command \"nope\" for \"kubectl\""
    );
}

#[test]
fn redirects_write_and_append_files_that_other_commands_read() {
    let mut e = engine();
    assert_eq!(e.eval("echo first > out/log.txt"), "");
    assert_eq!(e.eval("echo second >> out/log.txt"), "");
    assert_eq!(e.eval("cat out/log.txt"), "first\nsecond");
    assert_eq!(e.eval("echo -n one > out/log.txt"), "");
    assert_eq!(e.eval("cat out/log.txt | base64"), "b25l");
    assert_eq!(e.eval("ls out"), "log.txt");
    assert_eq!(e.eval("echo x > out"), "error: out: Is a directory");
    assert_eq!(e.eval("cat nope > copy.txt"), "cat: nope: No such file or directory");
    assert_eq!(e.eval("cat copy.txt"), "");
}

#[test]
fn a_live_object_can_be_exported_edited_and_replaced() {
    let mut e = engine();
    e.advance(10);
    assert_eq!(e.eval("kubectl get pod informant -o yaml > informant.yaml"), "");
    assert!(e.eval("cat informant.yaml").contains("name: informant"));
    assert_eq!(e.eval("kubectl delete pod informant"), "pod \"informant\" deleted");
    e.advance(40);
    assert_eq!(e.eval("kubectl create -f informant.yaml"), "pod/informant created");
}

#[test]
fn every_kubectl_stage_is_judged_and_a_failed_one_is_not() {
    let mut e = engine();
    assert_eq!(found(&e), 0);
    e.eval("kubectl get deployments | cat");
    assert_eq!(found(&e), 0);
    e.eval("kubectl get pods | grep nothing-matches");
    assert_eq!(found(&e), 1, "the command ran fine even if the filter printed nothing");

    let mut e = engine();
    e.eval("kubectl get pod nope | cat");
    assert_eq!(found(&e), 0);
}

#[test]
fn what_a_shell_would_do_but_this_one_cannot_is_refused_by_name() {
    let mut e = engine();
    assert_eq!(
        e.eval("echo $HOME"),
        "error: variable and command substitution is not available in this terminal"
    );
    assert_eq!(e.eval("echo a; echo b"), "error: `;` is not available in this terminal");
    assert_eq!(e.eval("echo a &&"), "error: `&` is not available in this terminal");
    assert_eq!(
        e.eval("cat < notes.txt"),
        "error: `<` is not available in this terminal"
    );
    assert_eq!(e.eval("echo a |"), "syntax error near unexpected token `newline'");
    assert_eq!(
        e.eval("kubectl get pods | | cat"),
        "syntax error near unexpected token `|'"
    );
    assert_eq!(e.eval("   "), "error: no command given");
}

#[test]
fn completion_leaves_everything_after_a_pipe_alone() {
    let e = engine();
    assert!(
        !e.complete("kubectl get pods").candidates.is_empty()
            || e.complete("kubectl g").candidates.contains(&"get".to_string())
    );
    assert!(e.complete("kubectl get pods | kubectl g").candidates.is_empty());
    assert!(e.complete("echo hi > ku").candidates.is_empty());
}
