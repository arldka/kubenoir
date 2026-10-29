//! Container processes are real programs: small template apps (`../apps`) compiled to
//! wasm32-wasip1, run by the wasmi interpreter at every container start. The run is simulated
//! ahead in one go against a virtual clock (sleeping costs nothing), and what the app printed,
//! and when and how it exited, becomes the `Program` the kubelet plays out on the cluster clock.
//!
//! The image's environment decides what the app does, so a changed environment (e.g. a fixed env
//! var after a rollout) changes what the next run does. Runs are deterministic: the same image,
//! config, environment and seed always give the same `Program`.

mod registry;
mod wasi;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wasmi::{Module, Store, TrapCode};

use crate::controllers::config::ImageSpec;
pub use registry::{is_reference, AppSet};
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
}

/// One container start.
pub struct Launch<'a> {
    pub image: &'a str,
    pub env: &'a BTreeMap<String, String>,
    /// Epoch seconds, for the app's wall clock.
    pub started_at: i64,
    /// Seeds the app's randomness.
    pub seed: u64,
}

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

/// Run `module` from `_start` to its exit, or until it is clearly running for good.
fn execute(module: &Module, args: Vec<String>, launch: &Launch<'_>) -> Program {
    let env = launch.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut store = Store::new(module.engine(), Process::new(args, env, launch.started_at, launch.seed));
    store.set_fuel(FUEL).expect("fuel metering is enabled");

    let outcome = wasi::linker(module)
        .and_then(|linker| linker.instantiate_and_start(&mut store, module))
        .and_then(|instance| instance.get_typed_func::<(), ()>(&store, "_start"))
        .and_then(|start| start.call(&mut store, ()));
    let code = match outcome {
        Ok(()) => Some(0),
        Err(e) if e.downcast_ref::<RunsForever>().is_some() => None,
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
                    seed: 0
                }
            ),
            Program::default()
        );
        assert_eq!(launch(&ImageSpec::default(), &[]), Program::default());
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
