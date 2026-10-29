//! The engine's half of `vim`: which files it may open, the request the terminal takes, and
//! reading back what the terminal saved (V24).

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"vim","files":["case/pod.yaml"],"cluster":{"now":"2026-10-27T09:00:00Z","manifests":["nodes.yaml"]},
    "findings":[{"id":"opened","clue":"You opened the case file.","when":{"command":{"verb":"vim","name":"case/pod.yaml"}}}]}"#;

fn engine() -> Engine {
    let files = Files::from([
        (
            "nodes.yaml".to_string(),
            "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
        ),
        ("case/pod.yaml".to_string(), "kind: Pod\n".to_string()),
    ]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

#[test]
fn vim_queues_a_request_for_the_terminal_once() {
    let mut e = engine();
    assert_eq!(e.take_editor_request(), None);
    assert_eq!(e.eval("vim case/pod.yaml"), "");
    assert_eq!(e.take_editor_request(), Some("case/pod.yaml".into()));
    assert_eq!(e.take_editor_request(), None, "taken once");
    assert_eq!(e.eval("vi ./case/pod.yaml"), "");
    assert_eq!(e.take_editor_request(), Some("case/pod.yaml".into()));
}

#[test]
fn a_new_file_may_be_opened_and_what_is_saved_is_read_back() {
    let mut e = engine();
    assert_eq!(e.eval("vim fresh.yaml"), "");
    assert_eq!(e.take_editor_request(), Some("fresh.yaml".into()));
    assert_eq!(e.read_file("fresh.yaml"), None, "nothing exists until the player saves");
    e.write_file("fresh.yaml", "kind: Namespace\n");
    assert_eq!(e.read_file("./fresh.yaml"), Some("kind: Namespace\n".into()));
    assert_eq!(e.eval("cat fresh.yaml"), "kind: Namespace");
}

#[test]
fn only_the_terminals_own_files_and_only_files() {
    let mut e = engine();
    for (line, message) in [
        (
            "vim /etc/passwd",
            "vim: /etc/passwd: files outside the terminal's own are not available; use a relative path",
        ),
        (
            "vim ../x",
            "vim: ../x: files outside the terminal's own are not available; use a relative path",
        ),
        (
            "vim ~/x",
            "vim: ~/x: files outside the terminal's own are not available; use a relative path",
        ),
        ("vim case", "vim: case: Is a directory"),
        ("vim .", "vim: .: Is a directory"),
        ("vim", "vim: no file given; usage: vim FILE"),
        ("vim a b", "vim: one file at a time; usage: vim FILE"),
    ] {
        assert_eq!(e.eval(line), message, "{line}");
        assert_eq!(e.take_editor_request(), None, "{line}");
    }
}

#[test]
fn the_editor_takes_the_whole_terminal() {
    let mut e = engine();
    for line in [
        "vim case/pod.yaml | cat",
        "vim case/pod.yaml > out.txt",
        "cat case/pod.yaml | vim x",
    ] {
        assert!(e.eval(line).contains("the editor takes the whole terminal"), "{line}");
        assert_eq!(e.take_editor_request(), None, "{line}");
    }
    assert!(e.read_file("out.txt").is_none_or(|t| t.is_empty()));
}

#[test]
fn opening_a_file_is_a_judged_command_and_changes_nothing_else() {
    let mut e = engine();
    let before = e.eval("kubectl get all");
    e.eval("vim case/pod.yaml");
    assert_eq!(e.read_file("case/pod.yaml"), Some("kind: Pod\n".into()));
    assert_eq!(e.eval("kubectl get all"), before);
    let status = serde_json::to_value(e.status()).unwrap();
    assert_eq!(status["findings"]["found"].as_array().unwrap().len(), 1);
    assert!(e.eval("vim --help").starts_with("Usage: vim FILE"));
}
