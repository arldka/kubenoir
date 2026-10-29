//! The clock every case runs against: fifteen minutes from the player's start, then the case
//! closes unsolved, saying what did not hold (V30); and every engine is its own simulation (V31).

mod common;

use engine::{AppSet, Engine, Files};
use serde_json::{json, Value};

fn status(e: &Engine) -> Value {
    serde_json::to_value(e.status()).unwrap()
}

#[test]
fn a_new_case_has_all_fifteen_minutes() {
    let e = common::engine("crashloop-env-typo");
    let s = status(&e);
    assert_eq!(s["timeLimitSeconds"], 900);
    assert_eq!(s["secondsLeft"], 900);
    assert_eq!(s["solved"], false);
    assert_eq!(s["outcome"], Value::Null);
}

#[test]
fn the_time_left_counts_down_with_the_cluster_clock() {
    let mut e = common::engine("crashloop-env-typo");
    e.advance(100);
    assert_eq!(status(&e)["secondsLeft"], 800);
    e.eval("sleep 5m");
    assert_eq!(status(&e)["secondsLeft"], 500);
}

#[test]
fn an_unsolved_case_closes_as_time_up_at_exactly_900_seconds() {
    let mut e = common::engine("crashloop-env-typo");
    e.advance(899);
    assert_eq!(status(&e)["solved"], false);
    e.advance(1);
    let s = status(&e);
    assert_eq!(s["solved"], true);
    assert_eq!(s["outcome"], "timeUp");
    assert_eq!(s["solvedAtSeconds"], 900);
    assert_eq!(s["elapsedSeconds"], 900);
    assert_eq!(s["secondsLeft"], 0);
}

#[test]
fn one_big_leap_still_stops_at_the_limit() {
    let mut e = common::engine("crashloop-env-typo");
    e.advance(5000);
    let s = status(&e);
    assert_eq!(s["outcome"], "timeUp");
    assert_eq!(s["elapsedSeconds"], 900);
}

#[test]
fn the_report_says_which_assertions_did_not_hold() {
    let mut e = common::engine("crashloop-env-typo");
    e.advance(900);
    let report = &status(&e)["report"];
    let assertions = report["assertions"].as_array().unwrap();
    assert_eq!(assertions.len(), 3);
    assert!(assertions.iter().all(|a| a["ok"] == false), "{assertions:?}");
    assert_eq!(report["missedFindings"].as_array().unwrap().len(), 3);
}

#[test]
fn a_partial_fix_is_shown_as_what_held() {
    let mut e = common::engine("crashloop-env-typo");
    // Fixes the Deployment's variable but the case then needs the 15 s to hold: do it late.
    e.advance(880);
    e.eval("kubectl set env deployment/payments-api DATABASE_URL=postgres://payments@postgres.default.svc.cluster.local:5432/payments DATABSE_URL-");
    e.advance(30);
    let s = status(&e);
    assert_eq!(
        s["outcome"], "timeUp",
        "the fix needed to hold 15 s but there were only 20 left"
    );
    let held: Vec<bool> = s["report"]["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ok"] == true)
        .collect();
    assert!(held.iter().any(|h| *h), "the new pod was ready by then: {held:?}");
}

#[test]
fn solving_in_time_is_not_affected() {
    let mut e = common::engine("crashloop-env-typo");
    e.eval("kubectl set env deployment/payments-api DATABASE_URL=postgres://payments@postgres.default.svc.cluster.local:5432/payments DATABSE_URL-");
    e.advance(60);
    let s = status(&e);
    assert_eq!(s["outcome"], "closed");
    assert!(s["secondsLeft"].as_i64().unwrap() > 800);
    // Time passing afterwards does not turn a solved case into a failed one.
    e.advance(2000);
    assert_eq!(status(&e)["outcome"], "closed");
}

#[test]
fn commands_after_the_end_do_not_count_or_unlock_anything() {
    let mut e = common::engine("crashloop-env-typo");
    e.advance(900);
    let commands = status(&e)["commands"].clone();
    e.eval("kubectl logs deploy/payments-api");
    assert_eq!(status(&e)["commands"], commands);
    assert_eq!(status(&e)["findings"]["found"], json!([]));
}

#[test]
fn the_limit_is_fixed_for_scenarios() {
    let (json, files) = common::scenario("crashloop-env-typo");
    let mut value: Value = serde_json::from_str(&json).unwrap();
    value["timeLimitMinutes"] = json!(30);
    let Err(err) = Engine::from_scenario(&value.to_string(), &files, AppSet::bundled()) else {
        panic!("a scenario with its own limit loaded");
    };
    let err = err.to_string();
    assert!(
        err.contains("timeLimitMinutes is not allowed") && err.contains("15 minutes"),
        "{err}"
    );
    let ok = Engine::from_scenario(&json, &files, AppSet::bundled());
    assert!(ok.is_ok());
}

#[test]
fn every_engine_is_a_fresh_simulation() {
    let (json, files): (String, Files) = common::scenario("crashloop-env-typo");
    let mut first = Engine::from_scenario(&json, &files, AppSet::bundled()).unwrap();
    first.eval("kubectl get pods");
    first.eval("kubectl logs deploy/payments-api");
    first.eval("kubectl delete deployment web");
    first.write_file("scratch.txt", "mine");
    first.advance(400);

    let second = Engine::from_scenario(&json, &files, AppSet::bundled()).unwrap();
    let s = status(&second);
    assert_eq!(s["secondsLeft"], 900);
    assert_eq!(s["commands"], 0);
    assert_eq!(s["findings"]["found"], json!([]));
    assert_eq!(second.now(), "2026-10-27T18:30:00Z");
    let mut second = second;
    assert!(second.eval("kubectl get deploy").contains("web"));
    assert_eq!(second.read_file("scratch.txt"), None);
    // The first one is untouched by the second.
    assert_eq!(status(&first)["commands"], 3);
}
