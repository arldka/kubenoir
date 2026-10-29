# Template apps

What a container does is decided by a real program: a template app in `apps/`, written in plain
Rust (`std::env::var`, `println!`, `std::thread::sleep`, `std::process::exit`) and built for
`wasm32-wasip1`. The engine runs one with the [wasmi](https://github.com/wasmi-labs/wasmi)
interpreter every time a container starts.

A scenario picks an app per image with a pinned reference, `"app": "db-client@0.1.0"`, and
configures it with `config` (see [scenario-format.md](scenario-format.md#images)). The version is
the one in the app's `Cargo.toml`.

Apps are artifacts, like container images:
- **In the browser,** the engine is handed apps fetched from an app registry: the local one that
  `make registry` builds, or a remote one (see [registry.md](registry.md)).
- **In native builds** (tests, `scenario-check`), the engine embeds the local builds instead
  (`engine/build.rs`, behind the default `bundled-apps` feature).

## How a run works

- **Arguments.** `argv[0]` is the image reference and `argv[1]` the image's `config`, as JSON.
- **Environment.** The container's literal `env` values. `valueFrom` entries are not resolved yet.
- **Output.** Every line written to stdout or stderr becomes a container log line, stamped with
  the simulated second it was written in.
- **Time is simulated.** The whole run is computed at container start against a virtual clock:
  `sleep` returns at once but moves the clock forward. The kubelet then plays the output out on
  the cluster clock, so `kubectl logs` shows each line only once its time has come.
- **Ending.** Returning from `main` exits with code 0, and `std::process::exit(n)` exits with `n`.
  A process that is still sleeping 30 days in (a server's `loop { sleep }`) runs forever. A panic
  or any other trap exits with 134, and a run past its instruction budget is killed with 137.
- **Determinism.** The same image, config, environment and start always produce the same run.
  Randomness is seeded per pod, container and restart, and the wall clock is the simulated one.
  Score verification replays sessions, so it relies on this.
- **What is available.** Only stdout/stderr, args, env, clocks, sleep, random and exit. There are
  no files, sockets or stdin.
- **Networking is mocked, not simulated.** An app that "connects" somewhere decides the outcome
  itself, by comparing what it was given with the servers its config describes (as `db-client`
  does with `database`). Nothing checks the cluster's Services or Pods.

## Templates

### `db-client`

A service that reads a PostgreSQL URL from its environment, connects, then serves.

| config | meaning |
|---|---|
| `name`, `version` | shown in the start-up line |
| `databaseUrlEnv` | variable holding the URL (default `DATABASE_URL`) |
| `portEnv` | variable holding the listen port (default `PORT`) |
| `database` | the one server that exists: `host`, `port`, `user`, `name`, optional `password` |

It logs `starting` and `loading configuration`, then one second later does one of the following:

| the URL | it logs | then |
|---|---|---|
| unset or empty | `ERROR required environment variable … is not set` | exits 1 |
| not `postgres://[user[:password]@]host[:port][/database]` | `ERROR invalid …` | exits 1 |
| another host or port (or no port) | `connecting to …`, `ERROR could not connect to …: connection refused` | exits 1 |
| right server, but another user, database or password | `connecting to …`, `ERROR the database server rejected the connection: unknown role or database` | exits 1 |
| exactly right | `connecting to …`, `connected to …`, `listening on :<port>` | serves |

Every failure ends with `FATAL cannot start without a database connection, exiting`. Missing parts
never fall back to defaults, so exactly one URL works.

### `nginx`

A healthy nginx. It prints its start-up lines and serves forever. It takes no config.

### `script`

Fixed output that ignores the environment. Use it for batch jobs, sidecars and filler.

```json
{ "logs": [ { "at": 0, "line": "migrating" }, { "at": 4, "line": "done" } ], "exit": { "at": 5, "code": 0 } }
```

Lines are printed at their `at` second (equal times keep their order). Without `exit` the process
keeps running.

## Adding a template

1. Add a crate under `apps/` (copy `nginx`), list it in `apps/Cargo.toml` `members`, and use `kit`
   for config, log lines and `serve_forever`. Any member with a `src/main.rs` is an app: the engine's
   build and `make registry` pick it up by itself.
2. Add tests in `engine/src/apps/mod.rs` that run it through the engine, and document it here.

## Changing an app

A published `name@version` never changes: recorded sessions must keep replaying to the same
verdict. To change what an app does, bump its `Cargo.toml` version and move scenarios to the new
reference when they are ready. The local registry only holds the current version of each app, so
locally a change shows up at once; a remote registry must keep every version it has published.

Keep config descriptive. If a template starts growing `when`/`then` keys, the behavior belongs in
its code instead.
