# Template apps

What a container does is decided by a real program: a template app in `apps/`, written in plain
Rust (`std::env::var`, `println!`, `std::thread::sleep`, `std::process::exit`) and built for
`wasm32-wasip1`. The engine runs one with the [wasmi](https://github.com/wasmi-labs/wasmi)
interpreter every time a container starts.

A scenario picks an app per image with a pinned reference, `"app": "db-client@0.2.0"`, and
configures it with `config` (see [scenario-format.md](scenario-format.md#images)). The version is
the one in the app's `Cargo.toml`.

Apps are artifacts, like container images:
- **In the browser,** the engine is handed apps fetched from an app registry: the local one that
  `make registry` builds, or a remote one (see [registry.md](registry.md)).
- **In native builds** (tests, `scenario-check`), the engine embeds the local builds instead
  (`engine/build.rs`, behind the default `bundled-apps` feature).

## How a run works

- **Arguments.** `argv[0]` is the image reference and `argv[1]` the image's `config`, as JSON.
- **Environment.** What the pod spec gives the container, resolved when it starts: `env` values (with `$(VAR)` expansion), `valueFrom` references to ConfigMap and Secret keys and to the pod's own fields, and `envFrom` sources. A missing ConfigMap, Secret or key keeps the container from starting (`CreateContainerConfigError`) until it exists. Secret values are decoded byte for byte, a trailing newline included.
- **Output.** Every line written to stdout or stderr becomes a container log line, stamped with
  the simulated second it was written in.
- **Time is simulated.** The whole run is computed at container start against a virtual clock:
  `sleep` returns at once but moves the clock forward. The kubelet then plays the output out on
  the cluster clock, so `kubectl logs` shows each line only once its time has come.
- **Ending.** Returning from `main` exits with code 0, and `std::process::exit(n)` exits with `n`.
  A process that is still sleeping 30 days in (a server's `loop { sleep }`) runs forever, and so
  does one that wakes up now and then and uses up its instruction budget (or logs 3000 lines, or
  changes files 3000 times) after simulating hours: only its first hours are played. A panic or any
  other trap exits with 134, and a loop that never sleeps is killed with 137.
- **Determinism.** The same image, config, environment and start always produce the same run.
  Randomness is seeded per pod, container and restart, and the wall clock is the simulated one.
  Score verification replays sessions, so it relies on this.
- **What is available.** stdout/stderr, args, env, clocks, sleep, random, exit and files. There are
  no sockets or stdin.
- **Files.** The pod's volumes are mounted at the paths its `volumeMounts` give, and the app reads
  and writes them with ordinary `std::fs` calls (the container's root is preopened at `/`).
  `configMap` and `secret` volumes are read-only (`Read-only file system`); `emptyDir` is
  writable and shared by the pod's containers, and its files survive a container restart but not
  the pod. A ConfigMap or Secret that changes reaches a running container about a minute later,
  except through a `subPath` mount, which keeps the content from when the container started. Other
  volume types keep the pod in `ContainerCreating` with a `FailedMount` event. Because a run is
  simulated ahead of time, the kubelet simulates a running app again whenever what it can read
  changes (a refresh, or a sibling's write); what it had already logged stays. Files are text.
- **Networking is answered by the cluster, not simulated.** No sockets exist. An app that
  connects asks the engine through one host call (`kit::ask`); the engine resolves the name from
  the pod's namespace, follows the Service and its ready endpoints, applies NetworkPolicies, checks
  that a program listens on the port, and has that program answer (see Servers below). The cluster
  is read when the container starts, so a fix shows up after the container restarts.

## Templates

### `db-client`

A service that reads a PostgreSQL URL from its environment, connects, then serves.

| config | meaning |
|---|---|
| `name`, `version` | shown in the start-up line |
| `databaseUrlEnv` | variable holding the URL (default `DATABASE_URL`) |
| `portEnv` | variable holding the listen port (default `PORT`) |
| `database` | optional, the earlier model: the one server that exists (`host`, `port`, `user`, `name`, optional `password`). Without it the server is whatever the cluster has at the URL's address (see below) |

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

Without `database`, the connection goes through the cluster: the URL's host is resolved from the
pod's namespace (short name, `svc.ns`, FQDN), through the Service and its ready endpoints, to the
program that listens on the port (a `postgres` pod). The client then tries to log in, so the
failures are the real ones: `could not translate host name … Name or service not known`,
`Connection refused` (no ready endpoint, or nothing listening), `timeout expired` (a port the
Service does not expose) or the server's own `FATAL:` line. The cluster is read when the container
starts: a database that comes up later is found by the next run, not the current one.

### `postgres`

A PostgreSQL server in the manner of the official image. `POSTGRES_PASSWORD` is required (without
it the container prints the image's error and exits 1); `POSTGRES_USER` defaults to `postgres` and
`POSTGRES_DB` to the user. It logs `ready to accept connections`, listens on `port` (config,
default 5432) and lets a client in only when role, database and password all match, answering
`FATAL:  role/database "x" does not exist` or `password authentication failed` otherwise.

### `echo-env`

Reports the environment it was started with: logs `<name> <version> starting`, then `NAME=value` for
each name in the config's `vars` (or `WARN NAME is not set`), then `reporting for duty`, and serves.
Config: `name`, `version`, `vars`.

### `nginx`

A healthy nginx. It prints its start-up lines and serves forever. It takes no config.

### `http-responder`

A small web service. It logs `<name> <version> starting (pid 1)`, then `listening on :<port>`, and
serves: `/` answers its banner, `/healthz` answers `ok`, anything else 404. The port is the
environment variable named by `portEnv` (default `defaultPort`, 8080); a value that is not a number
makes it exit 1. `WARMUP_SECS` (environment) makes it load for that long before it listens, which
is what a probe that fires too early trips over. For `exec` probes it understands the command
`true`. Config: `name`, `version`, `portEnv`, `defaultPort`.

### `watcher`

A service configured by files. At start it reads each file in `WATCH_FILES` (comma-separated
absolute paths) and logs `loaded <path>: <content>`; a file it cannot read is logged with the
reason, and fatal with `EXIT_ON_ERROR=1`. With `"reload": true` in the image config it re-reads
them every `WATCH_INTERVAL` seconds (default 30) and logs `reloaded <path>: ...` when one changed;
without it, it keeps what it read at start for good. Config: `name`, `version`, `reload`.

### `scribe`

A service that keeps a log file: it appends `entry N` to `SCRIBE_FILE` every `SCRIBE_INTERVAL`
seconds (default 10) and logs what it wrote or why it could not. `SCRIBE_COUNT=N` makes it stop
after N entries and exit cleanly. Config: `name`, `version`, `exitOnError` (give up when a write
fails).

### `script`

Fixed output that ignores the environment. Use it for batch jobs, sidecars and filler.

```json
{ "logs": [ { "at": 0, "line": "migrating" }, { "at": 4, "line": "done" } ], "exit": { "at": 5, "code": 0 } }
```

Lines are printed at their `at` second (equal times keep their order). Without `exit` the process
keeps running.

## Servers

A server app calls `kit::serve(port, handler)`. Nothing is bound and no socket exists: the call
declares the port to the engine (`Program.listens`, the host import `detective.listen`), which
uses it to tell an accepted connection from a refused one. When something connects, the engine runs
the same program again with `request <method> <path>` after the config; `serve` then prints
`status: <code>` and the body from the handler, and exits. So the answer comes from the app's code
and its environment, never from a table in the scenario.

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
