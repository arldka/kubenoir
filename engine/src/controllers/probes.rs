//! Probes, worked out when a container starts (C14, V18): the run is a program with known
//! timings, so every probe's outcome is a function of its schedule and of what the program does
//! at that moment. The plan says when the container becomes ready, which failed probes the
//! kubelet reports, and when a failing liveness or startup probe gets the container killed.

use k8s_openapi::api::core::v1::{Container, Probe};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use serde::{Deserialize, Serialize};

use crate::apps::{Program, Response};

/// A container with no readiness probe is ready this long after it starts.
pub const READY_DELAY_SECS: i64 = 1;
/// Probe schedules are followed this far, which is plenty for any container's life.
const MAX_PROBES: usize = 400;
const MAX_REPORTED: usize = 30;
/// The exit code of a container the kubelet killed (SIGKILL).
pub const KILLED: i32 = 137;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Offset (seconds after the start) the container becomes ready; `None` if it never does.
    pub ready_at: Option<i64>,
    /// Failed probes to report: offset, probe kind, reason.
    pub unhealthy: Vec<(i64, String, String)>,
    /// When the kubelet kills the container, and for which probe.
    pub kill: Option<(i64, String)>,
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            ready_at: Some(READY_DELAY_SECS),
            unhealthy: Vec::new(),
            kill: None,
        }
    }
}

/// How a program answers, as the probes see it.
pub struct Subject<'a> {
    pub program: &'a Program,
    pub pod_ip: &'a str,
    /// Ask the running program a question (method, path).
    pub ask: &'a dyn Fn(&str, &str) -> Response,
}

pub fn plan(container: &Container, subject: &Subject<'_>) -> Plan {
    let gone = subject.program.exit.map_or(i64::MAX, |(at, _)| at);
    let mut plan = Plan::default();
    let mut unhealthy = Vec::new();

    // The startup probe holds the other two back until it passes.
    let mut live_from = 0;
    if let Some(startup) = container.startup_probe.as_ref() {
        match run(startup, container, subject, 0, gone, "Startup", &mut unhealthy) {
            Run::Succeeded(at) => live_from = at,
            Run::Failed(at) => {
                plan.ready_at = None;
                plan.kill = Some((at, "startup".into()));
                plan.unhealthy = reported(unhealthy);
                return plan;
            }
            Run::Over => {
                plan.ready_at = None;
                plan.unhealthy = reported(unhealthy);
                return plan;
            }
        }
    }
    if let Some(readiness) = container.readiness_probe.as_ref() {
        plan.ready_at = match run(
            readiness,
            container,
            subject,
            live_from,
            gone,
            "Readiness",
            &mut unhealthy,
        ) {
            Run::Succeeded(at) => Some(at),
            // A readiness probe that keeps failing never kills the container.
            Run::Failed(_) | Run::Over => None,
        };
    } else if live_from > 0 {
        plan.ready_at = Some(live_from.max(READY_DELAY_SECS));
    }
    if let Some(liveness) = container.liveness_probe.as_ref() {
        if let Run::Failed(at) = run_liveness(liveness, container, subject, live_from, gone, &mut unhealthy) {
            plan.kill = Some((at, "liveness".into()));
        }
    }
    unhealthy.sort_by_key(|(at, _, _)| *at);
    plan.unhealthy = reported(unhealthy);
    plan
}

fn reported(mut all: Vec<(i64, String, String)>) -> Vec<(i64, String, String)> {
    all.truncate(MAX_REPORTED);
    all
}

enum Run {
    /// Enough consecutive successes, the last at this offset.
    Succeeded(i64),
    /// Enough consecutive failures, the last at this offset.
    Failed(i64),
    /// The container ended (or the schedule did) first.
    Over,
}

fn thresholds(probe: &Probe) -> (i64, i64, i32, i32) {
    (
        i64::from(probe.initial_delay_seconds.unwrap_or(0)),
        i64::from(probe.period_seconds.filter(|p| *p > 0).unwrap_or(10)),
        probe.failure_threshold.filter(|t| *t > 0).unwrap_or(3),
        probe.success_threshold.filter(|t| *t > 0).unwrap_or(1),
    )
}

/// Follow a probe from `from` until it has `successThreshold` successes or `failureThreshold`
/// failures in a row, reporting every failure on the way.
fn run(
    probe: &Probe,
    container: &Container,
    subject: &Subject<'_>,
    from: i64,
    gone: i64,
    kind: &str,
    unhealthy: &mut Vec<(i64, String, String)>,
) -> Run {
    let (delay, period, fail_at, pass_at) = thresholds(probe);
    let (mut passes, mut fails) = (0, 0);
    for k in 0..MAX_PROBES as i64 {
        let at = (from + delay).max(delay) + k * period;
        if at >= gone {
            return Run::Over;
        }
        match check(probe, container, subject, at) {
            Ok(()) => {
                (passes, fails) = (passes + 1, 0);
                if passes >= pass_at {
                    return Run::Succeeded(at);
                }
            }
            Err(reason) => {
                (passes, fails) = (0, fails + 1);
                unhealthy.push((at, kind.to_string(), reason));
                if fails >= fail_at {
                    return Run::Failed(at);
                }
            }
        }
    }
    Run::Over
}

/// Liveness never "succeeds" for good: it keeps probing, and the first run of `failureThreshold`
/// failures in a row is the one that kills.
fn run_liveness(
    probe: &Probe,
    container: &Container,
    subject: &Subject<'_>,
    from: i64,
    gone: i64,
    unhealthy: &mut Vec<(i64, String, String)>,
) -> Run {
    let (delay, period, fail_at, _) = thresholds(probe);
    let mut fails = 0;
    for k in 0..MAX_PROBES as i64 {
        let at = (from + delay).max(delay) + k * period;
        if at >= gone {
            return Run::Over;
        }
        match check(probe, container, subject, at) {
            Ok(()) => fails = 0,
            Err(reason) => {
                fails += 1;
                unhealthy.push((at, "Liveness".into(), reason));
                if fails >= fail_at {
                    return Run::Failed(at);
                }
            }
        }
    }
    Run::Over
}

fn listening_since(subject: &Subject<'_>, port: i64) -> Option<i64> {
    subject
        .program
        .listens
        .iter()
        .filter(|(_, p)| i64::from(*p) == port)
        .map(|(at, _)| *at)
        .min()
}

/// The container port a probe means: a number, or the name of one of the container's ports.
fn probe_port(port: &IntOrString, container: &Container) -> Option<i64> {
    match port {
        IntOrString::Int(n) => Some(i64::from(*n)),
        IntOrString::String(name) => container
            .ports
            .iter()
            .flatten()
            .find(|p| p.name.as_deref() == Some(name))
            .map(|p| i64::from(p.container_port)),
    }
}

/// One probe at offset `at`: `Ok` if the container answers, or why it does not.
fn check(probe: &Probe, container: &Container, subject: &Subject<'_>, at: i64) -> Result<(), String> {
    let refused = |port: i64, path: Option<&str>| {
        let dial = format!("dial tcp {}:{port}: connect: connection refused", subject.pod_ip);
        match path {
            Some(path) => format!("Get \"http://{}:{port}{path}\": {dial}", subject.pod_ip),
            None => dial,
        }
    };
    if let Some(http) = probe.http_get.as_ref() {
        let path = http.path.as_deref().unwrap_or("/");
        let Some(port) = probe_port(&http.port, container) else {
            return Err(format!("Get \"http://{}{path}\": invalid port", subject.pod_ip));
        };
        if listening_since(subject, port).is_none_or(|since| since > at) {
            return Err(refused(port, Some(path)));
        }
        let status = (subject.ask)("GET", path).status;
        return if (200..400).contains(&status) {
            Ok(())
        } else {
            Err(format!("HTTP probe failed with statuscode: {status}"))
        };
    }
    if let Some(tcp) = probe.tcp_socket.as_ref() {
        let Some(port) = probe_port(&tcp.port, container) else {
            return Err("invalid port".into());
        };
        return if listening_since(subject, port).is_some_and(|since| since <= at) {
            Ok(())
        } else {
            Err(refused(port, None))
        };
    }
    if let Some(exec) = probe.exec.as_ref() {
        let command = exec.command.clone().unwrap_or_default().join(" ");
        let reply = (subject.ask)("EXEC", &command);
        return if reply.status == 200 {
            Ok(())
        } else {
            Err(format!("command \"{command}\" exited with 1: {}", reply.body))
        };
    }
    Ok(())
}
