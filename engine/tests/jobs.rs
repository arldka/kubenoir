//! End-to-end: Jobs and CronJobs (V17). Pods run to completion, failures are retried until the
//! backoff limit is passed, and CronJobs fire on the virtual clock.

use engine::{AppSet, Engine, Files};

// The clock starts at 09:00:00 UTC.
const SCENARIO: &str = r#"{"schemaVersion":2,"id":"jobs","cluster":{"now":"2026-10-27T09:00:00Z","manifests":["nodes.yaml"]},
    "images":{
        "ok:1":{"pullSeconds":1,"app":"script@0.1.0","config":{"logs":[{"at":0,"line":"report done"}],"exit":{"at":3,"code":0}}},
        "bad:1":{"pullSeconds":1,"app":"script@0.1.0","config":{"logs":[{"at":0,"line":"disk full"}],"exit":{"at":2,"code":1}}},
        "slow:1":{"pullSeconds":1,"app":"script@0.1.0","config":{"logs":[{"at":0,"line":"working"}],"exit":{"at":400,"code":0}}}}}"#;

fn engine() -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn apply(e: &mut Engine, yaml: &str) -> String {
    e.write_file("apply.yaml", yaml);
    e.eval("kubectl apply -f apply.yaml")
}

fn job(name: &str, image: &str, extra: &str) -> String {
    format!(
        "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: {name}\nspec:\n{extra}  template:\n    spec:\n      restartPolicy: Never\n      containers:\n      - name: main\n        image: {image}\n"
    )
}

fn cronjob(schedule: &str, image: &str, extra: &str) -> String {
    format!(
        "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: shift\nspec:\n  schedule: '{schedule}'\n{extra}  jobTemplate:\n    spec:\n      template:\n        spec:\n          restartPolicy: Never\n          containers:\n          - name: main\n            image: {image}\n"
    )
}

fn status(e: &mut Engine, name: &str) -> Vec<String> {
    e.eval(&format!("kubectl get job {name} --no-headers"))
        .split_whitespace()
        .map(String::from)
        .collect()
}

fn job_names(e: &mut Engine) -> Vec<String> {
    e.eval("kubectl get jobs -o name")
        .lines()
        .filter(|l| !l.starts_with("No resources"))
        .map(|l| l.trim_start_matches("job.batch/").to_string())
        .collect()
}

#[test]
fn a_job_runs_its_pod_to_completion() {
    let mut e = engine();
    assert_eq!(apply(&mut e, &job("report", "ok:1", "")), "job.batch/report created");
    e.advance(2);
    assert_eq!(status(&mut e, "report")[1], "Running");
    e.advance(10);
    let row = status(&mut e, "report");
    assert_eq!(&row[1..3], ["Complete", "1/1"], "{row:?}");
    let pods = e.eval("kubectl get pods --no-headers");
    assert!(pods.contains("Completed") && pods.starts_with("report-"), "{pods}");
    assert_eq!(e.eval("kubectl logs job/report"), "report done");
    assert!(e
        .eval("kubectl describe job report")
        .contains("0 Active (0 Ready) / 1 Succeeded / 0 Failed"));
}

#[test]
fn failures_are_retried_until_the_backoff_limit_is_passed() {
    let mut e = engine();
    apply(&mut e, &job("flaky", "bad:1", "  backoffLimit: 2\n"));
    e.advance(5);
    assert_eq!(status(&mut e, "flaky")[1], "Running", "one failure is not the end");
    let describe = e.eval("kubectl describe job flaky");
    assert!(
        describe.contains("0 Active (0 Ready) / 0 Succeeded / 1 Failed") || describe.contains("1 Active"),
        "{describe}"
    );
    // Failures at about +3 s, +15 s, +27 s: the third is more than backoffLimit (2).
    e.advance(60);
    let row = status(&mut e, "flaky");
    assert_eq!(row[1], "Failed", "{row:?}");
    let describe = e.eval("kubectl describe job flaky");
    assert!(
        describe.contains("0 Active (0 Ready) / 0 Succeeded / 3 Failed"),
        "{describe}"
    );
    assert!(
        describe.contains("BackoffLimitExceeded") && describe.contains("Job has reached the specified backoff limit"),
        "{describe}"
    );
    let pods = e.eval("kubectl get pods --no-headers");
    assert_eq!(pods.lines().filter(|l| l.contains("Error")).count(), 3, "{pods}");
    // No more pods appear.
    e.advance(120);
    assert_eq!(e.eval("kubectl get pods --no-headers").lines().count(), 3);
}

#[test]
fn the_deadline_fails_a_job_and_stops_its_pod() {
    let mut e = engine();
    apply(&mut e, &job("stuck", "slow:1", "  activeDeadlineSeconds: 30\n"));
    e.advance(20);
    assert_eq!(status(&mut e, "stuck")[1], "Running");
    e.advance(15);
    assert_eq!(status(&mut e, "stuck")[1], "Failed");
    assert!(e.eval("kubectl describe job stuck").contains("DeadlineExceeded"));
    e.advance(10);
    assert!(!e.eval("kubectl get pods --no-headers").contains("Running"));
}

#[test]
fn completions_and_parallelism() {
    let mut e = engine();
    apply(&mut e, &job("batch", "ok:1", "  completions: 4\n  parallelism: 2\n"));
    e.advance(2);
    assert_eq!(e.eval("kubectl get pods --no-headers").lines().count(), 2);
    e.advance(40);
    let row = status(&mut e, "batch");
    assert_eq!(&row[1..3], ["Complete", "4/4"], "{row:?}");
}

#[test]
fn a_job_must_not_restart_forever() {
    let mut e = engine();
    let always = "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: x\nspec:\n  template:\n    spec:\n      containers:\n      - {name: m, image: 'ok:1'}\n";
    let out = apply(&mut e, always);
    assert!(
        // Defaulted to Always, which upstream reports as a missing value.
        out.contains("spec.template.spec.restartPolicy: Required value: valid values: \"OnFailure\", \"Never\""),
        "{out}"
    );
}

#[test]
fn create_job_runs_a_command_once() {
    let mut e = engine();
    assert_eq!(
        e.eval("kubectl create job once --image=ok:1 -- echo hi"),
        "job.batch/once created"
    );
    e.advance(10);
    assert_eq!(status(&mut e, "once")[1], "Complete");
    assert_eq!(e.eval("kubectl delete job once"), "job.batch \"once\" deleted");
    // Its pod goes in the background, as after deleting a Deployment.
    e.advance(5);
    assert_eq!(e.eval("kubectl get pods"), "No resources found in default namespace.");
}

#[test]
fn a_cronjob_fires_on_its_schedule_on_the_virtual_clock() {
    let mut e = engine();
    assert_eq!(
        apply(&mut e, &cronjob("*/5 * * * *", "ok:1", "")),
        "cronjob.batch/shift created"
    );
    let row = e.eval("kubectl get cronjob shift --no-headers");
    assert!(row.contains("*/5 * * * *") && row.contains("<none>"), "{row}");
    e.advance(4 * 60);
    assert!(job_names(&mut e).is_empty());
    e.advance(70);
    // 09:05:00 is 1793092500 s; the Job is named after the scheduled minute.
    assert_eq!(job_names(&mut e), ["shift-29884865"]);
    let row = e.eval("kubectl get cronjob shift --no-headers");
    assert!(row.contains("false"), "{row}");
    e.advance(5 * 60);
    assert_eq!(job_names(&mut e).len(), 2);
    assert!(e
        .eval("kubectl describe cronjob shift")
        .contains("Schedule:                      */5 * * * *"));
}

#[test]
fn a_wrong_schedule_is_rejected_and_midnight_utc_is_far_away() {
    let mut e = engine();
    let out = apply(&mut e, &cronjob("every 5 minutes", "ok:1", ""));
    assert!(
        out.contains("spec.schedule: Invalid value: \"every 5 minutes\""),
        "{out}"
    );
    // The player wanted every five minutes and wrote midnight: nothing for a long time.
    apply(&mut e, &cronjob("0 0 * * *", "ok:1", ""));
    e.advance(3600);
    assert!(job_names(&mut e).is_empty());
    // The player meant ten o'clock. Waiting is a command, not a lecture.
    apply(&mut e, &cronjob("0 10 * * *", "ok:1", ""));
    assert_eq!(e.eval("sleep 1h"), "");
    assert_eq!(job_names(&mut e).len(), 1);
}

#[test]
fn concurrency_policy_decides_what_a_slow_job_means_for_the_next_one() {
    let policies = [("Allow", 2, 2), ("Forbid", 1, 1), ("Replace", 1, 1)];
    for (policy, jobs, running_pods) in policies {
        let mut e = engine();
        apply(
            &mut e,
            &cronjob("*/5 * * * *", "slow:1", &format!("  concurrencyPolicy: {policy}\n")),
        );
        e.advance(5 * 60 + 10 * 60 / 6);
        e.advance(5 * 60);
        let names = job_names(&mut e);
        assert_eq!(names.len(), jobs, "{policy}: {names:?}");
        let pods = e.eval("kubectl get pods --no-headers");
        assert_eq!(
            pods.lines().filter(|l| l.contains("Running")).count(),
            running_pods,
            "{policy}: {pods}"
        );
    }
}

#[test]
fn suspended_cronjobs_do_not_fire_and_history_is_trimmed() {
    let mut e = engine();
    apply(&mut e, &cronjob("* * * * *", "ok:1", "  suspend: true\n"));
    e.advance(300);
    assert!(job_names(&mut e).is_empty());

    let mut e = engine();
    apply(
        &mut e,
        &cronjob("* * * * *", "ok:1", "  successfulJobsHistoryLimit: 2\n"),
    );
    e.advance(10 * 60);
    let names = job_names(&mut e);
    assert_eq!(names.len(), 3, "two finished kept and maybe one running: {names:?}");
    assert_eq!(e.eval("kubectl get pods --no-headers").lines().count(), 3);
}

#[test]
fn deleting_a_cronjob_takes_its_jobs_and_pods_along() {
    let mut e = engine();
    apply(&mut e, &cronjob("*/5 * * * *", "ok:1", ""));
    e.advance(6 * 60);
    assert_eq!(job_names(&mut e).len(), 1);
    e.eval("kubectl delete cronjob shift");
    e.advance(30);
    assert!(job_names(&mut e).is_empty());
    assert_eq!(e.eval("kubectl get pods"), "No resources found in default namespace.");
    e.advance(600);
    assert!(job_names(&mut e).is_empty(), "nothing fires any more");
}

#[test]
fn sleep_validates_its_argument() {
    let mut e = engine();
    assert!(e.eval("sleep").starts_with("sleep: missing operand"));
    assert!(e.eval("sleep soon").starts_with("sleep: invalid time interval 'soon'"));
    let before = e.now();
    e.eval("sleep 90");
    assert_eq!(e.now(), "2026-10-27T09:01:30Z");
    assert_eq!(before, "2026-10-27T09:00:00Z");
}
