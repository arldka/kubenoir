//! End-to-end: files a scenario seeds into the terminal, read with `cat` and `ls`.

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"files",
    "files":["notes/case.txt","notes/web.yaml","todo.txt"],
    "cluster":{"now":"2026-10-27T18:30:00Z"},
    "findings":[{"id":"read","clue":"you read the case notes","when":{"command":{"verb":"cat","name":"notes/case.txt"}}}]}"#;

fn engine() -> Engine {
    let files: Files = [
        ("notes/case.txt", "The butler did it.\n"),
        (
            "notes/web.yaml",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: clue\n",
        ),
        ("todo.txt", "buy milk\n"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn found(e: &Engine) -> usize {
    serde_json::to_value(e.status()).unwrap()["findings"]["found"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn the_terminal_starts_with_the_scenarios_files() {
    let mut e = engine();
    assert_eq!(e.eval("ls"), "notes\ntodo.txt");
    assert_eq!(e.eval("ls notes"), "case.txt\nweb.yaml");
    assert_eq!(e.eval("cat todo.txt"), "buy milk");
    assert_eq!(e.eval("cat notes/case.txt todo.txt"), "The butler did it.\nbuy milk");
}

#[test]
fn seeded_files_can_be_applied_and_player_files_listed() {
    let mut e = engine();
    assert_eq!(e.eval("kubectl apply -f notes/web.yaml"), "configmap/clue created");
    e.write_file("mine.yaml", "x");
    assert_eq!(e.eval("ls"), "mine.yaml\nnotes\ntodo.txt");
}

#[test]
fn failures_and_options_read_like_coreutils() {
    let mut e = engine();
    assert_eq!(e.eval("cat nope.txt"), "cat: nope.txt: No such file or directory");
    assert_eq!(e.eval("cat notes"), "cat: notes: Is a directory");
    assert_eq!(e.eval("ls nope"), "ls: cannot access 'nope': No such file or directory");
    assert_eq!(e.eval("ls -l"), "ls: invalid option -- 'l'");
    assert_eq!(e.eval("rm todo.txt"), "rm: command not found");
}

#[test]
fn reading_a_file_unlocks_findings_only_when_it_worked() {
    let mut e = engine();
    e.eval("cat notes/nope.txt");
    e.eval("cat todo.txt");
    assert_eq!(found(&e), 0);
    e.eval("cat notes/case.txt");
    assert_eq!(found(&e), 1);
}
