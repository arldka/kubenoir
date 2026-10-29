//! Full playthrough of the `crashloop-env-typo` scenario through the public engine API: the
//! investigation (get, logs, describe, events) and the fix (set env, then watch the rollout).

mod common;

use engine::{AppSet, Engine, Files};

const ID: &str = "crashloop-env-typo";
const DATABASE_URL: &str = "postgres://payments@postgres.default.svc.cluster.local:5432/payments";

fn engine() -> Engine {
    common::engine(ID)
}

fn scenario() -> (String, Files) {
    common::scenario(ID)
}

/// Name of the pod whose name starts with `prefix` and is not terminating.
fn pod_named(e: &mut Engine, prefix: &str) -> String {
    let out = e.eval("kubectl get pods -o name");
    out.lines()
        .map(|l| l.trim_start_matches("pod/"))
        .find(|n| n.starts_with(prefix))
        .unwrap_or_else(|| panic!("no pod {prefix}* in:\n{out}"))
        .to_string()
}

fn payments_rows(e: &mut Engine) -> Vec<Vec<String>> {
    e.eval("kubectl get pods")
        .lines()
        .skip(1)
        .filter(|l| l.starts_with("payments-api-"))
        .map(|l| l.split_whitespace().map(String::from).collect())
        .collect()
}

#[test]
fn the_player_starts_47_minutes_into_a_crash_loop() {
    let mut e = engine();
    assert_eq!(e.now(), "2026-10-27T18:30:00Z");
    let pods = e.eval("kubectl get pods");
    let web: Vec<_> = pods.lines().filter(|l| l.starts_with("web-")).collect();
    assert_eq!(web.len(), 2, "{pods}");
    assert!(web.iter().all(|l| l.contains("1/1") && l.contains("Running")), "{pods}");

    let payments = payments_rows(&mut e);
    assert_eq!(payments.len(), 1, "{pods}");
    assert_eq!(payments[0][1..], ["0/1", "CrashLoopBackOff", "256", "47m"]);

    assert!(e.eval("kubectl get pods -n kube-system").contains("etcd-control-plane"));
}

#[test]
fn the_logs_name_the_missing_variable() {
    let mut e = engine();
    let pod = pod_named(&mut e, "payments-api-");
    let logs = e.eval(&format!("kubectl logs {pod}"));
    assert_eq!(
        logs,
        "INFO  payments-api 2.4.1 starting (pid 1)\n\
         INFO  loading configuration from environment\n\
         ERROR required environment variable DATABASE_URL is not set\n\
         FATAL cannot start without a database connection, exiting"
    );
    assert_eq!(e.eval(&format!("kubectl logs {pod} --previous")), logs);
    assert_eq!(
        e.eval("kubectl logs deploy/payments-api --tail 1"),
        "FATAL cannot start without a database connection, exiting"
    );
}

#[test]
fn describe_and_events_point_at_the_typo_and_the_backoff() {
    let mut e = engine();
    let pod = pod_named(&mut e, "payments-api-");
    let described = e.eval(&format!("kubectl describe pod {pod}"));
    for expected in [
        "    State:          Waiting",
        "      Reason:       CrashLoopBackOff",
        "    Last State:     Terminated",
        "      Exit Code:    1",
        "    Restart Count:  256",
        "      PORT:         8080",
        "      DATABSE_URL:  postgres://payments@postgres.default.svc.cluster.local:5432/payments",
    ] {
        assert!(
            described.lines().any(|l| l == expected),
            "missing {expected:?} in:\n{described}"
        );
    }
    assert!(
        !described.contains("DATABASE_URL:"),
        "the correctly spelled variable must not exist"
    );

    let events = e.eval("kubectl get events");
    let backoff = events
        .lines()
        .find(|l| l.contains("BackOff"))
        .unwrap_or_else(|| panic!("{events}"));
    assert!(
        backoff.contains("Warning") && backoff.contains("(x256 over 4"),
        "{backoff}"
    );
    assert!(backoff.contains(&format!("pod/{pod}")), "{backoff}");
    assert!(events.contains("Scaled up replica set payments-api-"), "{events}");
}

#[test]
fn fixing_the_variable_name_brings_the_service_back() {
    let mut e = engine();
    let broken = pod_named(&mut e, "payments-api-");

    let out = e.eval(&format!(
        "kubectl set env deployment/payments-api DATABASE_URL={DATABASE_URL} DATABSE_URL-"
    ));
    assert_eq!(out, "deployment.apps/payments-api env updated");

    // The rollout starts immediately: a new pod appears next to the crashing one.
    let rows = payments_rows(&mut e);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().any(|r| r[0] != broken && r[2] == "ContainerCreating"),
        "{rows:?}"
    );

    e.advance(3 + 1); // image pull + one second running -> Ready
    let rows = payments_rows(&mut e);
    assert!(rows.iter().any(|r| r[0] == broken && r[2] == "Terminating"), "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r[0] != broken && r[1] == "1/1" && r[2] == "Running"),
        "{rows:?}"
    );

    e.advance(2);
    let rows = payments_rows(&mut e);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0][1..4], ["1/1", "Running", "0"]);

    assert_eq!(
        e.eval("kubectl logs deploy/payments-api"),
        format!(
            "INFO  payments-api 2.4.1 starting (pid 1)\n\
             INFO  loading configuration from environment\n\
             INFO  connecting to {DATABASE_URL}\n\
             INFO  connected to {DATABASE_URL}\n\
             INFO  listening on :8080"
        )
    );
    assert!(e.eval("kubectl get deployment payments-api").contains("1/1"));
    // The web tier was never touched.
    let pods = e.eval("kubectl get pods");
    assert_eq!(
        pods.lines()
            .filter(|l| l.starts_with("web-") && l.contains("1/1"))
            .count(),
        2,
        "{pods}"
    );
}

#[test]
fn a_misspelled_fix_changes_nothing_for_the_better() {
    let mut e = engine();
    let broken = pod_named(&mut e, "payments-api-");
    e.eval(&format!(
        "kubectl set env deployment/payments-api DATABASE_URI={DATABASE_URL}"
    ));
    e.advance(600);
    let rows = payments_rows(&mut e);
    assert_eq!(
        rows.len(),
        2,
        "the old pod must stay until a healthy replacement exists: {rows:?}"
    );
    assert!(rows.iter().all(|r| r[2] == "CrashLoopBackOff"), "{rows:?}");
    assert!(rows.iter().any(|r| r[0] == broken));
    assert!(e.eval("kubectl get deploy payments-api").contains("0/1"));
}

#[test]
fn pods_cannot_be_edited_in_place() {
    let mut e = engine();
    let pod = pod_named(&mut e, "payments-api-");
    let out = e.eval(&format!("kubectl set env pod/{pod} DATABASE_URL=x"));
    assert!(
        out.starts_with(&format!(
            "The Pod \"{pod}\" is invalid: spec: Forbidden: pod updates may not change fields other than"
        )),
        "{out}"
    );
}

#[test]
fn set_env_list_and_unknown_targets() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl set env deploy/payments-api --list"),
        format!(
            "# deployments/payments-api, container payments-api\nPORT=8080\nLOG_LEVEL=info\nDATABSE_URL={DATABASE_URL}"
        )
    );
    assert_eq!(
        e.eval("kubectl set env deploy/nope A=B"),
        "Error from server (NotFound): deployments.apps \"nope\" not found"
    );
    assert_eq!(
        e.eval("kubectl set env deploy/payments-api A=B -c nope"),
        "error: unable to find container named \"nope\""
    );
    assert_eq!(
        e.eval("kubectl set env deploy/payments-api"),
        "error: at least one environment variable must be provided"
    );
}

#[test]
fn deleting_a_pod_makes_the_replicaset_start_a_replacement() {
    let mut e = engine();
    let victim = pod_named(&mut e, "web-");
    assert_eq!(
        e.eval(&format!("kubectl delete pod {victim}")),
        format!("pod \"{victim}\" deleted")
    );

    let pods = e.eval("kubectl get pods");
    assert!(
        pods.lines()
            .any(|l| l.starts_with(&victim) && l.contains("Terminating")),
        "{pods}"
    );
    assert_eq!(
        pods.lines()
            .filter(|l| l.starts_with("web-") && !l.contains("Terminating"))
            .count(),
        2,
        "{pods}"
    );

    e.advance(3);
    let pods = e.eval("kubectl get pods");
    assert!(!pods.contains(&victim), "{pods}");
    assert_eq!(
        pods.lines()
            .filter(|l| l.starts_with("web-") && l.contains("1/1"))
            .count(),
        2,
        "{pods}"
    );

    assert_eq!(
        e.eval("kubectl delete pod nope"),
        "Error from server (NotFound): pods \"nope\" not found"
    );
}

#[test]
fn force_deleting_removes_the_pod_at_once() {
    let mut e = engine();
    let victim = pod_named(&mut e, "web-");
    let out = e.eval(&format!("kubectl delete pod {victim} --force --grace-period=0"));
    assert!(
        out.starts_with("Warning: Immediate deletion") && out.ends_with(&format!("pod \"{victim}\" deleted")),
        "{out}"
    );
    assert!(!e.eval("kubectl get pods").contains(&victim));
}

#[test]
fn rollout_restart_replaces_every_pod_of_the_deployment() {
    let mut e = engine();
    let before: Vec<String> = e
        .eval("kubectl get pods -o name")
        .lines()
        .filter(|l| l.contains("/web-"))
        .map(String::from)
        .collect();
    assert_eq!(
        e.eval("kubectl rollout restart deployment/web"),
        "deployment.apps/web restarted"
    );
    e.advance(30);
    let after: Vec<String> = e
        .eval("kubectl get pods -o name")
        .lines()
        .filter(|l| l.contains("/web-"))
        .map(String::from)
        .collect();
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(after.iter().all(|p| !before.contains(p)), "{before:?} -> {after:?}");
    assert!(
        e.eval("kubectl get rs")
            .lines()
            .filter(|l| l.starts_with("web-"))
            .count()
            >= 2
    );
    assert_eq!(
        e.eval("kubectl rollout restart deployment/nope"),
        "Error from server (NotFound): deployments.apps \"nope\" not found"
    );
    assert_eq!(
        e.eval("kubectl rollout restart pod/x"),
        "error: pods \"x\" cannot be restarted"
    );
}

#[test]
fn deleting_a_deployment_removes_its_replicasets_and_pods() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl delete deployment web"),
        "deployment.apps \"web\" deleted"
    );
    e.advance(5);
    let pods = e.eval("kubectl get pods");
    assert!(!pods.contains("web-"), "{pods}");
    assert!(!e.eval("kubectl get rs").contains("web-"));
    assert_eq!(
        e.eval("kubectl get deploy web"),
        "Error from server (NotFound): deployments.apps \"web\" not found"
    );
    assert!(pods.contains("payments-api-"));
}

#[test]
fn the_same_inputs_always_replay_to_the_same_cluster() {
    let script = [
        "kubectl get pods",
        "kubectl set env deployment/payments-api DATABASE_URL=postgres://x DATABSE_URL-",
        "@advance 4",
        "kubectl get pods -o wide",
        "@advance 2",
        "kubectl get rs -o yaml",
        "kubectl get events -o yaml",
        "kubectl logs deploy/payments-api",
        "kubectl describe deployment payments-api",
    ];
    let run = || {
        let mut e = engine();
        script
            .iter()
            .map(|step| match step.strip_prefix("@advance ") {
                Some(secs) => {
                    e.advance(secs.parse().unwrap());
                    e.now()
                }
                None => e.eval(step),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn many_small_advances_equal_one_big_one() {
    let mut big = engine();
    big.advance(900);
    let mut small = engine();
    for _ in 0..900 {
        small.advance(1);
    }
    for cmd in [
        "kubectl get pods -o yaml",
        "kubectl get events -o yaml",
        "kubectl get rs -o yaml",
        "kubectl get deploy -o yaml",
    ] {
        assert_eq!(big.eval(cmd), small.eval(cmd), "{cmd}");
    }
}

// ---- success criteria ---------------------------------------------------------------------

use serde_json::{json, Value};

fn status(e: &Engine) -> Value {
    serde_json::to_value(e.status()).unwrap()
}

const FIX: &str = "kubectl set env deployment/payments-api DATABASE_URL=postgres://payments@postgres.default.svc.cluster.local:5432/payments DATABSE_URL-";

#[test]
fn the_case_starts_open_with_hidden_findings_to_discover() {
    let e = engine();
    let s = status(&e);
    assert_eq!(s["solved"], false);
    assert_eq!(s["outcome"], Value::Null);
    assert_eq!(s["findings"]["total"], 3);
    assert_eq!(s["findings"]["found"], json!([]));
    assert_eq!(s["commands"], 0);
    assert_eq!(
        s["report"],
        Value::Null,
        "hidden rule labels must not leak before the case is closed"
    );
}

#[test]
fn a_correct_fix_closes_the_case_once_it_has_held_for_the_stability_window() {
    let mut e = engine();
    e.eval(FIX);
    // New pod Ready at +4 s, the old one is gone at +6 s, and the state must then hold for 15 s.
    e.advance(5 + 15);
    assert_eq!(status(&e)["solved"], false, "one second short of the window");
    e.advance(1);
    let s = status(&e);
    assert_eq!(s["solved"], true);
    assert_eq!(s["outcome"], "closed");
    assert_eq!(s["solvedAtSeconds"], 21);
    assert_eq!(s["elapsedSeconds"], 21);
    assert_eq!(s["commands"], 1);

    let report = &s["report"];
    assert_eq!(report["assertions"].as_array().unwrap().len(), 3);
    assert!(report["assertions"].as_array().unwrap().iter().all(|a| a["ok"] == true));
    assert!(report["constraints"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["ok"] == true));
    // Nothing was investigated, so every clue is revealed as missed.
    assert_eq!(report["missedFindings"].as_array().unwrap().len(), 3);
}

#[test]
fn the_verdict_does_not_depend_on_how_time_is_advanced() {
    let mut big = engine();
    big.eval(FIX);
    big.advance(300);
    let mut small = engine();
    small.eval(FIX);
    for _ in 0..300 {
        small.advance(1);
    }
    assert_eq!(status(&big), status(&small));
    assert_eq!(status(&big)["solvedAtSeconds"], 21);
}

#[test]
fn a_wrong_fix_never_closes_the_case() {
    let mut e = engine();
    e.eval("kubectl set env deployment/payments-api DATABASE_URI=postgres://x");
    e.advance(900);
    assert_eq!(status(&e)["solved"], false);
}

#[test]
fn doing_nothing_never_closes_the_case() {
    let mut e = engine();
    e.advance(3600);
    assert_eq!(status(&e)["solved"], false);
}

#[test]
fn restarting_the_web_tier_closes_the_case_at_a_cost() {
    let mut e = engine();
    e.eval("kubectl rollout restart deployment/web");
    e.eval(FIX);
    e.advance(120);
    let s = status(&e);
    assert_eq!(s["outcome"], "closedWithCost");
    let constraints = s["report"]["constraints"].as_array().unwrap();
    let broken: Vec<&str> = constraints
        .iter()
        .filter(|c| c["ok"] == false)
        .map(|c| c["label"].as_str().unwrap())
        .collect();
    assert_eq!(broken, ["the web deployment was left alone"]);
    assert_eq!(constraints[0]["brokenAtSeconds"], 0);
}

#[test]
fn deleting_the_deployment_is_forbidden_even_if_it_is_then_restored() {
    let mut e = engine();
    // Wiping it out and re-creating it cannot be done with these commands, but the attempt counts
    // against the player the moment it succeeds.
    e.eval("kubectl delete deployment payments-api");
    let s = status(&e);
    assert_eq!(s["solved"], false);
    assert_eq!(s["commands"], 1);
}

#[test]
fn investigating_unlocks_findings_one_by_one() {
    let mut e = engine();
    let pod = pod_named(&mut e, "payments-api-");

    e.eval("kubectl get pods");
    assert_eq!(status(&e)["findings"]["found"], json!([]));

    e.eval(&format!("kubectl logs {pod}"));
    let s = status(&e);
    assert_eq!(s["findings"]["found"].as_array().unwrap().len(), 1);
    assert_eq!(s["findings"]["found"][0]["id"], "logs");
    assert!(s["findings"]["found"][0]["clue"]
        .as_str()
        .unwrap()
        .contains("DATABASE_URL is not set"));
    assert_eq!(s["findings"]["found"][0]["atSeconds"], 0);

    e.advance(30);
    e.eval(&format!("kubectl describe pod {pod}")); // unlocks both the events and the environment clues
    let ids: Vec<String> = status(&e)["findings"]["found"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["logs", "events", "environment"]);
    assert_eq!(status(&e)["findings"]["found"][1]["atSeconds"], 30);

    // Asking again changes nothing.
    e.eval("kubectl get events");
    assert_eq!(status(&e)["findings"]["found"].as_array().unwrap().len(), 3);
}

#[test]
fn commands_that_fail_do_not_unlock_findings() {
    let mut e = engine();
    e.eval("kubectl logs payments-api-nope");
    e.eval("kubectl get events -n ghost-namespace-that-has-none -o json");
    let s = status(&e);
    assert_eq!(
        s["findings"]["found"].as_array().unwrap().len(),
        1,
        "only the empty-but-valid events query counts"
    );
    assert_eq!(s["commands"], 2);
}

#[test]
fn a_recorded_session_replays_to_the_same_verdict() {
    // Pod names are deterministic; look one up on a scratch engine so `live` runs only the session.
    let pod = pod_named(&mut engine(), "payments-api-");
    let mut live = engine();
    let steps = json!([
        { "kind": "eval", "command": format!("kubectl logs {pod}") },
        { "kind": "advance", "seconds": 7 },
        { "kind": "hint", "index": 0 },
        { "kind": "eval", "command": FIX },
        { "kind": "advance", "seconds": 40 },
    ]);
    live.eval(&format!("kubectl logs {pod}"));
    live.advance(7);
    live.eval(FIX);
    live.advance(40);

    let parsed: Vec<engine::Step> = serde_json::from_value(steps).unwrap();
    let (json, files) = scenario();
    let replayed = Engine::replay(&json, &files, &parsed, AppSet::bundled()).unwrap();
    assert_eq!(serde_json::to_value(replayed).unwrap(), status(&live));
    assert_eq!(status(&live)["solved"], true);
}

#[test]
fn broken_rules_in_a_scenario_are_rejected_with_a_helpful_message() {
    let (json, files) = scenario();
    let mut scenario: Value = serde_json::from_str(&json).unwrap();
    scenario["success"]["assert"][0]["select"]["kind"] = json!("Pods");
    let err = Engine::from_scenario(&scenario.to_string(), &files, AppSet::bundled())
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("success.assert[0]") && err.contains("unknown kind \"Pods\""),
        "{err}"
    );
}

#[test]
fn the_backoff_event_count_always_equals_the_restart_count() {
    let mut e = engine();
    // Sample the pod while it is running, crashing and waiting in backoff.
    for step in 0..40 {
        e.advance(if step == 0 { 1 } else { 37 });
        let restarts: u64 = payments_rows(&mut e)[0][3].parse().unwrap();
        let events: Value = serde_json::from_str(&e.eval("kubectl get events -o json")).unwrap();
        let backoffs: u64 = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|ev| {
                ev["reason"] == "BackOff"
                    && ev["involvedObject"]["name"]
                        .as_str()
                        .unwrap()
                        .starts_with("payments-api-")
            })
            .map(|ev| ev["count"].as_u64().unwrap())
            .sum();
        assert_eq!(backoffs, restarts, "at +{}s", e.now());
    }
}

// ---- the value must be the right one ------------------------------------------------------

fn set_url(e: &mut Engine, value: &str) -> String {
    e.eval(&format!("kubectl set env deployment/payments-api DATABASE_URL={value}"))
}

/// Logs of every payments pod, joined (the old crashing pod and the new one).
fn all_payments_logs(e: &mut Engine) -> String {
    let names: Vec<String> = e
        .eval("kubectl get pods -o name")
        .lines()
        .filter(|l| l.contains("/payments-api-"))
        .map(|l| l.trim_start_matches("pod/").to_string())
        .collect();
    names
        .iter()
        .map(|n| e.eval(&format!("kubectl logs {n}")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_wrong_database_url_never_brings_the_service_back() {
    let right_host_wrong_db = "postgres://payments@postgres.default.svc.cluster.local:5432/wrong";
    let with_slash = format!("{DATABASE_URL}/");
    let upper = DATABASE_URL.to_uppercase();
    for wrong in [
        "x",
        "postgres://x",
        right_host_wrong_db,
        with_slash.as_str(),
        upper.as_str(),
    ] {
        let mut e = engine();
        assert_eq!(set_url(&mut e, wrong), "deployment.apps/payments-api env updated");
        e.advance(900);

        let rows = payments_rows(&mut e);
        assert_eq!(
            rows.len(),
            2,
            "{wrong}: the old pod must stay while the new one crashes: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r[2] == "CrashLoopBackOff" && r[1] == "0/1"),
            "{wrong}: {rows:?}"
        );
        let s = status(&e);
        assert_eq!(s["solved"], false, "{wrong}");
        assert!(e.eval("kubectl get deployment payments-api").contains("0/1"), "{wrong}");
    }
}

#[test]
fn the_logs_say_what_went_wrong_with_the_value() {
    let mut e = engine();
    set_url(&mut e, "postgres://x");
    e.advance(30);
    let logs = all_payments_logs(&mut e);
    assert!(logs.contains("connecting to postgres://x"), "{logs}");
    assert!(
        logs.contains("could not connect to postgres://x: connection refused"),
        "{logs}"
    );

    // The right server but the wrong database or user is told apart from a wrong server.
    let mut e = engine();
    set_url(
        &mut e,
        "postgres://payments@postgres.default.svc.cluster.local:5432/wrong",
    );
    e.advance(30);
    let logs = all_payments_logs(&mut e);
    assert!(logs.contains("the database server rejected the connection"), "{logs}");
    assert!(!logs.contains("connection refused"), "{logs}");
}

#[test]
fn the_exact_value_is_required_but_a_rename_that_copies_the_old_value_works() {
    let mut e = engine();
    // What a careful player does: read the misspelled variable's value, then reuse it.
    let listing = e.eval("kubectl set env deploy/payments-api --list");
    let copied = listing
        .lines()
        .find_map(|l| l.strip_prefix("DATABSE_URL="))
        .expect("the typo'd variable is listed");
    assert_eq!(copied, DATABASE_URL);

    e.eval(&format!(
        "kubectl set env deploy/payments-api DATABASE_URL={copied} DATABSE_URL-"
    ));
    e.advance(60);
    let s = status(&e);
    assert_eq!(
        (s["solved"].clone(), s["outcome"].clone()),
        (json!(true), json!("closed"))
    );
    assert_eq!(s["solvedAtSeconds"], 21);
}

#[test]
fn the_old_misspelled_variable_may_stay_once_the_right_one_is_set() {
    let mut e = engine();
    set_url(&mut e, DATABASE_URL);
    e.advance(60);
    assert_eq!(status(&e)["solved"], true);
    assert!(e
        .eval("kubectl set env deploy/payments-api --list")
        .contains("DATABSE_URL="));
}

#[test]
fn a_wrong_value_is_not_solved_by_waiting_and_the_right_one_afterwards_still_is() {
    let mut e = engine();
    set_url(&mut e, "postgres://x");
    e.advance(600);
    assert_eq!(status(&e)["solved"], false);
    set_url(&mut e, DATABASE_URL);
    e.advance(600);
    let s = status(&e);
    assert_eq!(s["solved"], true);
    assert_eq!(s["outcome"], "closed");
}
