//! Container processes are real programs: small template apps (`../apps`) compiled to
//! wasm32-wasip1, run by the wasmi interpreter at every container start. The run is simulated
//! ahead in one go against a virtual clock (sleeping costs nothing), and what the app printed,
//! and when and how it exited, becomes the `Program` the kubelet plays out on the cluster clock.
//!
//! The image's environment decides what the app does, so a changed environment (e.g. a fixed env
//! var after a rollout) changes what the next run does. Runs are deterministic: the same image,
//! config, environment and seed always give the same `Program`.

mod registry;
pub mod vfs;
mod wasi;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wasmi::{Module, Store, TrapCode};

use crate::controllers::config::ImageSpec;
pub use registry::{is_reference, AppSet};
pub use vfs::{Mount, Timeline, VolEvent};
use wasi::{Process, RunsForever};

/// Instructions one run may execute before it is killed, so a runaway app cannot hang the game.
const FUEL: u64 = 20_000_000;
/// Exit code of a process that aborted (a Rust panic, any other trap), as for SIGABRT.
const EXIT_ABORTED: i32 = 134;
/// Exit code of a process killed for running out of fuel, as for SIGKILL.
const EXIT_KILLED: i32 = 137;

/// What one run of a container does, with offsets in seconds after it starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// Log lines sorted by offset.
    pub lines: Vec<(i64, String)>,
    /// `(offset, exit code)`; `None` means the process runs forever.
    pub exit: Option<(i64, i32)>,
    /// The files the run wrote or removed, stamped with the second (epoch) it did.
    #[serde(default)]
    pub writes: Vec<VolEvent>,
    /// `(offset, port)` for every port the app declared it listens on.
    #[serde(default)]
    pub listens: Vec<(i64, u16)>,
}

/// What a listening app answers to one request.
// Nothing asks yet: `kubectl exec … curl` (T22) will.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// One container start.
pub struct Launch<'a> {
    pub image: &'a str,
    pub env: &'a BTreeMap<String, String>,
    /// Epoch seconds, for the app's wall clock.
    pub started_at: i64,
    /// Seeds the app's randomness.
    pub seed: u64,
    /// The volumes mounted into the container.
    pub mounts: &'a [Mount],
    /// What those volumes hold over time.
    pub timeline: &'a Timeline,
}

/// Nothing in any volume, for tests of apps that have no volumes.
#[cfg(test)]
pub static NO_TIMELINE: Timeline = Timeline {
    base: BTreeMap::new(),
    events: Vec::new(),
};

/// Run the image's app for one container start. An image without an app is a silent process
/// that never exits. (Scenario loading guarantees every referenced app is in `apps`.)
pub fn run(apps: &AppSet, spec: Option<&ImageSpec>, launch: &Launch<'_>) -> Program {
    let Some(spec) = spec else {
        return Program::default();
    };
    let Some(module) = spec.app.as_deref().and_then(|app| apps.get(app)) else {
        return Program::default();
    };

    let config = if spec.config.is_null() {
        "{}".to_string()
    } else {
        spec.config.to_string()
    };
    execute(module, vec![launch.image.to_string(), config], launch)
}

#[allow(dead_code)]
/// Ask the image's app to answer one request, as a fresh run of the same program in request mode
/// (`argv[2] == "request"`, then the method and path). The handler prints `status: <code>` and
/// then the body. An app that is not a server, or one that dies on the request, gives a 500.
pub fn request(apps: &AppSet, spec: Option<&ImageSpec>, launch: &Launch<'_>, method: &str, path: &str) -> Response {
    let failed = |body: &str| Response {
        status: 500,
        body: body.to_string(),
    };
    let Some(spec) = spec else {
        return failed("no handler");
    };
    let Some(module) = spec.app.as_deref().and_then(|app| apps.get(app)) else {
        return failed("no handler");
    };
    let config = if spec.config.is_null() {
        "{}".to_string()
    } else {
        spec.config.to_string()
    };
    let args = vec![
        launch.image.to_string(),
        config,
        "request".to_string(),
        method.to_string(),
        path.to_string(),
    ];
    let program = execute(module, args, launch);
    // The app's start-up output comes first; the answer starts at the `status:` line.
    let lines: Vec<&str> = program.lines.iter().map(|(_, l)| l.as_str()).collect();
    let answer = lines.iter().position(|l| l.starts_with("status: "));
    let status = match (program.exit, answer) {
        (Some((_, 0)), Some(at)) => lines[at]["status: ".len()..].parse().ok(),
        _ => None,
    };
    match status {
        Some(status) => Response {
            status,
            body: lines[answer.unwrap_or(0) + 1..].join("\n"),
        },
        None => failed("the handler did not answer"),
    }
}

/// Run `module` from `_start` to its exit, or until it is clearly running for good.
fn execute(module: &Module, args: Vec<String>, launch: &Launch<'_>) -> Program {
    let env = launch.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut store = Store::new(
        module.engine(),
        Process::new(
            args,
            env,
            launch.started_at,
            launch.seed,
            launch.mounts,
            launch.timeline,
        ),
    );
    store.set_fuel(FUEL).expect("fuel metering is enabled");

    let outcome = wasi::linker(module)
        .and_then(|linker| linker.instantiate_and_start(&mut store, module))
        .and_then(|instance| instance.get_typed_func::<(), ()>(&store, "_start"))
        .and_then(|start| start.call(&mut store, ()));
    let code = match outcome {
        Ok(()) => Some(0),
        Err(e) if e.downcast_ref::<RunsForever>().is_some() => None,
        // A service that wakes up now and then and ran out of fuel after simulating hours of
        // that is still running; a loop that never sleeps is killed.
        Err(e) if e.as_trap_code() == Some(TrapCode::OutOfFuel) && store.data().elapsed_secs() > 0 => None,
        Err(e) => Some(e.i32_exit_status().unwrap_or_else(|| {
            if e.as_trap_code() == Some(TrapCode::OutOfFuel) {
                EXIT_KILLED
            } else {
                EXIT_ABORTED
            }
        })),
    };

    let process = store.data_mut();
    process.flush();
    Program {
        lines: std::mem::take(&mut process.lines),
        exit: code.map(|code| (process.elapsed_secs(), code)),
        writes: std::mem::take(&mut process.fs.writes),
        listens: std::mem::take(&mut process.listens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DATABASE_URL: &str = "postgres://payments@postgres.default.svc.cluster.local:5432/payments";

    fn spec(app: &str, config: serde_json::Value) -> ImageSpec {
        ImageSpec {
            app: Some(app.into()),
            config,
            ..ImageSpec::default()
        }
    }

    fn db_client() -> ImageSpec {
        spec(
            "db-client@0.1.0",
            json!({
                "name": "payments-api", "version": "2.4.1",
                "database": {"host": "postgres.default.svc.cluster.local", "port": 5432,
                             "user": "payments", "name": "payments"}
            }),
        )
    }

    fn launch(spec: &ImageSpec, env: &[(&str, &str)]) -> Program {
        let env: BTreeMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        run(
            &AppSet::bundled(),
            Some(spec),
            &Launch {
                image: "img:1",
                env: &env,
                started_at: 1_000,
                seed: 7,
                mounts: &[],
                timeline: &NO_TIMELINE,
            },
        )
    }

    fn lines(p: &Program) -> Vec<&str> {
        p.lines.iter().map(|(_, l)| l.as_str()).collect()
    }

    #[test]
    fn db_client_connects_with_the_right_url_and_serves() {
        let p = launch(&db_client(), &[("DATABASE_URL", DATABASE_URL), ("PORT", "8080")]);
        assert_eq!(
            p.lines,
            [
                (0, "INFO  payments-api 2.4.1 starting (pid 1)".to_string()),
                (0, "INFO  loading configuration from environment".to_string()),
                (1, format!("INFO  connecting to {DATABASE_URL}")),
                (1, format!("INFO  connected to {DATABASE_URL}")),
                (1, "INFO  listening on :8080".to_string()),
            ]
        );
        assert_eq!(p.exit, None);
    }

    #[test]
    fn db_client_without_the_variable_crashes_after_a_second() {
        for env in [&[("DATABSE_URL", DATABASE_URL)][..], &[("DATABASE_URL", "")]] {
            let p = launch(&db_client(), env);
            assert_eq!(
                p.lines[2..],
                [
                    (
                        1,
                        "ERROR required environment variable DATABASE_URL is not set".to_string()
                    ),
                    (
                        1,
                        "FATAL cannot start without a database connection, exiting".to_string()
                    ),
                ]
            );
            assert_eq!(p.exit, Some((1, 1)));
        }
    }

    #[test]
    fn db_client_tells_apart_what_is_wrong_with_the_url() {
        let error = |url: &str| {
            let p = launch(&db_client(), &[("DATABASE_URL", url)]);
            assert_eq!(p.exit, Some((1, 1)), "{url}");
            assert_eq!(
                lines(&p).last().copied(),
                Some("FATAL cannot start without a database connection, exiting")
            );
            lines(&p)[lines(&p).len() - 2].to_string()
        };
        assert!(error("x").starts_with("ERROR invalid DATABASE_URL \"x\""));
        let refused = "connection refused (check the host and port)";
        for wrong_server in [
            "postgres://x",
            "postgres://payments@postgres.default.svc.cluster.local/payments",
            "postgres://payments@postgres.default.svc.cluster.local:5433/payments",
            "postgres://payments@POSTGRES.default.svc.cluster.local:5432/payments",
        ] {
            assert!(error(wrong_server).ends_with(refused), "{wrong_server}");
        }
        let rejected = "ERROR the database server rejected the connection: unknown role or database";
        let with_slash = format!("{DATABASE_URL}/");
        for wrong_login in [
            "postgres://payments@postgres.default.svc.cluster.local:5432/wrong",
            "postgres://postgres.default.svc.cluster.local:5432/payments",
            "postgres://payments:secret@postgres.default.svc.cluster.local:5432/payments",
            "postgres://payments@postgres.default.svc.cluster.local:5432",
            with_slash.as_str(),
        ] {
            assert_eq!(error(wrong_login), rejected, "{wrong_login}");
        }
        assert!(error(&DATABASE_URL.to_uppercase()).starts_with("ERROR invalid"));
    }

    #[test]
    fn nginx_starts_and_serves() {
        let p = launch(&spec("nginx@0.1.0", serde_json::Value::Null), &[]);
        assert_eq!(
            p.lines,
            [
                (
                    0,
                    "/docker-entrypoint.sh: Configuration complete; ready for start up".to_string()
                ),
                (1, "nginx: worker process started".to_string()),
            ]
        );
        assert_eq!(p.exit, None);
    }

    #[test]
    fn script_plays_lines_in_time_order_then_exits() {
        let config = json!({"logs": [{"at": 3, "line": "late"}, {"line": "first"}, {"line": "second"}],
                            "exit": {"at": 5, "code": 3}});
        let p = launch(&spec("script@0.1.0", config), &[]);
        assert_eq!(
            p.lines,
            [
                (0, "first".to_string()),
                (0, "second".to_string()),
                (3, "late".to_string())
            ]
        );
        assert_eq!(p.exit, Some((5, 3)));

        let forever = launch(&spec("script@0.1.0", json!({"logs": [{"line": "up"}]})), &[]);
        assert_eq!(forever.exit, None);
        assert_eq!(
            launch(&spec("script@0.1.0", json!({"exit": {"code": 0}})), &[]).exit,
            Some((0, 0))
        );
    }

    #[test]
    fn bad_config_exits_with_code_2() {
        let p = launch(&spec("db-client@0.1.0", json!({"name": 1})), &[]);
        assert_eq!(p.exit, Some((0, 2)));
        assert!(lines(&p)[0].starts_with("FATAL invalid image config"), "{p:?}");
    }

    #[test]
    fn no_spec_or_no_app_is_silent_and_immortal() {
        assert_eq!(
            run(
                &AppSet::bundled(),
                None,
                &Launch {
                    image: "x",
                    env: &BTreeMap::new(),
                    started_at: 0,
                    seed: 0,
                    mounts: &[],
                    timeline: &NO_TIMELINE,
                }
            ),
            Program::default()
        );
        assert_eq!(launch(&ImageSpec::default(), &[]), Program::default());
    }

    fn mount(path: &str, volume: &str, read_only: bool) -> Mount {
        Mount {
            path: path.into(),
            volume: volume.into(),
            sub_path: String::new(),
            read_only,
            frozen: false,
        }
    }

    fn volumes(volume: &str, files: &[(&str, &str)]) -> vfs::Volumes {
        BTreeMap::from([(
            volume.to_string(),
            files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        )])
    }

    fn launch_with(spec: &ImageSpec, env: &[(&str, &str)], mounts: &[Mount], timeline: &Timeline) -> Program {
        let env: BTreeMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        run(
            &AppSet::bundled(),
            Some(spec),
            &Launch {
                image: "img:1",
                env: &env,
                started_at: 1_000,
                seed: 7,
                mounts,
                timeline,
            },
        )
    }

    fn watcher(reload: bool) -> ImageSpec {
        spec(
            "watcher@0.1.0",
            json!({"name": "profiler", "version": "1.0", "reload": reload}),
        )
    }

    #[test]
    fn the_watcher_reads_a_mounted_file_and_with_reload_notices_when_it_changes() {
        let timeline = Timeline {
            base: volumes("cfg", &[("mode", "fast\n")]),
            events: vec![VolEvent {
                at: 1_060,
                volume: "cfg".into(),
                path: "mode".into(),
                content: Some("slow\n".into()),
                append: false,
            }],
        };
        let mounts = [mount("/etc/config", "cfg", true)];
        let env = [("WATCH_FILES", "/etc/config/mode"), ("WATCH_INTERVAL", "30")];

        let p = launch_with(&watcher(true), &env, &mounts, &timeline);
        assert_eq!(p.exit, None);
        assert_eq!(
            p.lines,
            [
                (0, "INFO  profiler 1.0 starting (pid 1)".to_string()),
                (0, "INFO  loaded /etc/config/mode: fast".to_string()),
                (60, "INFO  reloaded /etc/config/mode: slow".to_string()),
            ]
        );

        // Without hot reload the service keeps what it read at start.
        let p = launch_with(&watcher(false), &env, &mounts, &timeline);
        assert_eq!(
            lines(&p),
            [
                "INFO  profiler 1.0 starting (pid 1)",
                "INFO  loaded /etc/config/mode: fast"
            ]
        );
    }

    #[test]
    fn a_container_started_after_the_change_reads_the_new_content() {
        let timeline = Timeline {
            base: volumes("cfg", &[("mode", "fast\n")]),
            events: vec![VolEvent {
                at: 999,
                volume: "cfg".into(),
                path: "mode".into(),
                content: Some("slow\n".into()),
                append: false,
            }],
        };
        let p = launch_with(
            &watcher(false),
            &[("WATCH_FILES", "/etc/config/mode")],
            &[mount("/etc/config", "cfg", true)],
            &timeline,
        );
        assert!(lines(&p).contains(&"INFO  loaded /etc/config/mode: slow"));
    }

    #[test]
    fn missing_files_are_logged_with_the_reason_and_can_be_fatal() {
        let mounts = [mount("/etc/config", "cfg", true)];
        let timeline = Timeline {
            base: volumes("cfg", &[]),
            events: vec![],
        };
        let p = launch_with(
            &watcher(false),
            &[("WATCH_FILES", "/etc/config/nope,/elsewhere/x")],
            &mounts,
            &timeline,
        );
        assert_eq!(
            lines(&p)[1..],
            [
                "ERROR cannot read /etc/config/nope: No such file or directory",
                "ERROR cannot read /elsewhere/x: No such file or directory"
            ]
        );
        assert_eq!(p.exit, None);
        let p = launch_with(
            &watcher(false),
            &[("WATCH_FILES", "/etc/config/nope"), ("EXIT_ON_ERROR", "1")],
            &mounts,
            &timeline,
        );
        assert_eq!(p.exit, Some((0, 1)));
        assert!(lines(&p)
            .last()
            .unwrap()
            .contains("cannot start without /etc/config/nope"));
        let p = launch_with(&watcher(false), &[], &mounts, &timeline);
        assert_eq!(p.exit, Some((0, 1)));
        assert!(lines(&p).contains(&"FATAL required environment variable WATCH_FILES is not set"));
    }

    #[test]
    fn the_scribe_appends_to_scratch_space_and_reports_what_it_wrote() {
        let scribe = spec("scribe@0.1.0", json!({"name": "scribe", "version": "1.0"}));
        let mounts = [mount("/data", "scratch", false)];
        let timeline = Timeline {
            base: volumes("scratch", &[]),
            events: vec![],
        };
        let env = [("SCRIBE_FILE", "/data/app.log"), ("SCRIBE_INTERVAL", "10")];
        let p = launch_with(&scribe, &env, &mounts, &timeline);
        assert_eq!(p.exit, None);
        assert_eq!(
            &lines(&p)[1..4],
            [
                "INFO  wrote entry 1 to /data/app.log",
                "INFO  wrote entry 2 to /data/app.log",
                "INFO  wrote entry 3 to /data/app.log"
            ]
        );
        assert_eq!(p.lines[2].0, 10, "ten seconds apart");
        // Each append is an event with the file's whole content at that second.
        assert_eq!(p.writes[0].at, 1_000);
        assert_eq!(p.writes[0].content.as_deref(), Some("entry 1\n"));
        // After the file exists each entry is just the text added at the end.
        assert_eq!(
            (p.writes[2].content.as_deref(), p.writes[2].append),
            (Some("entry 3\n"), true)
        );
        assert_eq!(p.writes[2].at, 1_020);

        // A restarted container carries on the same file.
        let timeline = Timeline {
            base: volumes("scratch", &[("app.log", "entry 1\nentry 2\n")]),
            events: vec![],
        };
        let again = launch_with(&scribe, &env, &mounts, &timeline);
        assert_eq!(
            (again.writes[0].content.as_deref(), again.writes[0].append),
            (Some("entry 1\n"), true)
        );
    }

    #[test]
    fn writing_to_a_read_only_mount_fails_with_the_real_reason() {
        let scribe = spec("scribe@0.1.0", json!({"name": "scribe", "version": "1.0"}));
        let mounts = [mount("/etc/config", "cfg", true)];
        let timeline = Timeline {
            base: volumes("cfg", &[("x", "1")]),
            events: vec![],
        };
        let env = [("SCRIBE_FILE", "/etc/config/app.log")];
        let p = launch_with(&scribe, &env, &mounts, &timeline);
        assert_eq!(
            lines(&p)[1],
            "ERROR cannot write /etc/config/app.log: Read-only file system"
        );
        assert!(p.writes.is_empty());
        let strict = spec("scribe@0.1.0", json!({"name": "scribe", "exitOnError": true}));
        let p = launch_with(&strict, &env, &mounts, &timeline);
        assert_eq!(p.exit, Some((0, 1)));
        // And a path outside every mount does not exist at all.
        let p = launch_with(&scribe, &[("SCRIBE_FILE", "/nowhere/app.log")], &mounts, &timeline);
        assert_eq!(
            lines(&p)[1],
            "ERROR cannot write /nowhere/app.log: No such file or directory"
        );
    }

    #[test]
    fn apps_that_do_not_use_files_are_unaffected_by_having_volumes() {
        let timeline = Timeline {
            base: volumes("cfg", &[("a", "1")]),
            events: vec![],
        };
        let with = launch_with(
            &db_client(),
            &[("DATABASE_URL", DATABASE_URL)],
            &[mount("/etc/c", "cfg", true)],
            &timeline,
        );
        assert_eq!(with, launch(&db_client(), &[("DATABASE_URL", DATABASE_URL)]));
    }

    /// Run a hand-written module that imports `fd_write` (as `write`) and `proc_exit`.
    fn raw(body: &str) -> Program {
        let wat = format!(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
                (memory (export "memory") 1)
                (data (i32.const 16) "dying\n")
                (func (export "_start") {body}))"#
        );
        let engine = AppSet::bundled().get("script@0.1.0").unwrap().engine().clone();
        let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
        let env = BTreeMap::new();
        let launch = Launch {
            image: "raw",
            env: &env,
            started_at: 0,
            seed: 0,
            mounts: &[],
            timeline: &NO_TIMELINE,
        };
        execute(&module, vec![], &launch)
    }

    /// `fd_write(1, iovs = [{16, 6}] at 0, written at 8)`: prints "dying".
    const PRINT: &str = "(i32.store (i32.const 0) (i32.const 16)) (i32.store (i32.const 4) (i32.const 6))
                         (drop (call $write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))";

    #[test]
    fn a_trap_aborts_with_134_and_keeps_what_was_printed() {
        let p = raw(&format!("{PRINT} unreachable"));
        assert_eq!(p.lines, [(0, "dying".to_string())]);
        assert_eq!(p.exit, Some((0, EXIT_ABORTED)));
    }

    #[test]
    fn a_runaway_loop_is_killed_with_137() {
        assert_eq!(raw("(loop (br 0))").exit, Some((0, EXIT_KILLED)));
    }

    #[test]
    fn exit_codes_and_returning_from_start() {
        assert_eq!(raw("(call $exit (i32.const 42))").exit, Some((0, 42)));
        assert_eq!(raw("").exit, Some((0, 0)));
    }

    #[test]
    fn runs_are_deterministic() {
        let env = [("DATABASE_URL", "postgres://x")];
        assert_eq!(launch(&db_client(), &env), launch(&db_client(), &env));
    }
}

#[cfg(test)]
mod server_tests {
    use super::*;
    use serde_json::json;

    fn responder() -> ImageSpec {
        ImageSpec {
            app: Some("http-responder@0.1.0".into()),
            config: json!({"name": "informant", "version": "1.2", "portEnv": "PORT"}),
            ..ImageSpec::default()
        }
    }

    fn launch<'a>(env: &'a BTreeMap<String, String>) -> Launch<'a> {
        Launch {
            image: "informant:1",
            env,
            started_at: 1_000,
            seed: 1,
            mounts: &[],
            timeline: &NO_TIMELINE,
        }
    }

    fn env(port: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("PORT".to_string(), port.to_string())])
    }

    #[test]
    fn a_server_declares_the_port_it_listens_on() {
        let env = env("3000");
        let program = run(&AppSet::bundled(), Some(&responder()), &launch(&env));
        assert_eq!(program.listens, [(1, 3000)]);
        assert_eq!(program.exit, None);
        assert_eq!(program.lines.last().unwrap().1, "INFO  listening on :3000");
    }

    #[test]
    fn a_server_that_cannot_start_declares_nothing() {
        let env = env("eighty");
        let program = run(&AppSet::bundled(), Some(&responder()), &launch(&env));
        assert!(program.listens.is_empty());
        assert_eq!(program.exit, Some((0, 1)));
    }

    #[test]
    fn requests_are_answered_by_the_app_itself() {
        let apps = AppSet::bundled();
        let env = env("3000");
        let ask = |path| request(&apps, Some(&responder()), &launch(&env), "GET", path);
        assert_eq!(
            ask("/"),
            Response {
                status: 200,
                body: "informant 1.2".into()
            }
        );
        assert_eq!(
            ask("/healthz"),
            Response {
                status: 200,
                body: "ok".into()
            }
        );
        assert_eq!(ask("/nope").status, 404);
    }

    #[test]
    fn an_app_that_is_not_a_server_does_not_answer() {
        let apps = AppSet::bundled();
        let env = BTreeMap::new();
        let spec = ImageSpec {
            app: Some("nginx@0.1.0".into()),
            ..ImageSpec::default()
        };
        assert_eq!(request(&apps, Some(&spec), &launch(&env), "GET", "/").status, 500);
    }
}
