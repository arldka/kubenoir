# Registries: where scenarios and apps come from

The game loads its case (a scenario) and the apps that case's containers run over HTTP. The same
layout is served by the **local registry**, a folder of static files built from this repository,
and by any **remote** backend. One client (`frontend/src/lib/registry.ts`) talks to both.

## Layout

```
{scenarioSource}/scenarios/index.json        [{ "id", "title", "objective" }]
{scenarioSource}/scenarios/{id}.json         the scenario file
{appRegistry}/apps/index.json                { "db-client": ["0.1.0"], ... }
{appRegistry}/apps/{name}/{version}.wasm     the app (wasm32-wasip1)
```

- **References.** Scenarios reference apps as `name@version` (`"app": "db-client@0.2.0"`).
- **Immutable versions.** A published version must never change, because recorded sessions are
  replayed to verify scores and need the exact same apps. Publish a new version instead.
- **Safe path segments.** Ids, names and versions use only `A-Z a-z 0-9 . _ -`.
- **CORS.** A remote registry on another origin must send CORS headers (for example
  `Access-Control-Allow-Origin: *`), or the browser cannot read it.

## The local registry

`make registry` builds it into `frontend/static/registry/` (gitignored), from this repository's
sources. `make dev`, `make build`, `make check` and `make e2e` run it first.

1. **Apps.** It builds every app in `apps/` (`make apps`) and publishes each under the version in its
   `Cargo.toml`.
2. **Check.** It runs `scenario-check` on every scenario: each must load, and its `solution.json`, if
   any, must solve it.
3. **Scenarios.** It publishes each `scenarios/{id}/scenario.json` as `scenarios/{id}.json` and lists
   it in the index. A file without a `briefing` is an engine test fixture and is skipped. The folder
   must be named after the scenario's `id`, and every app the scenario references must be in the
   registry.

## Pointing the game at backends

These are build-time settings (Vite env vars, e.g. in `frontend/.env.local`):

| variable | meaning | default |
|---|---|---|
| `VITE_SCENARIO_SOURCES` | comma-separated base URLs serving `scenarios/` | the local registry |
| `VITE_APP_REGISTRIES` | comma-separated base URLs serving `apps/` | the local registry |
| `VITE_PROGRESS_URL` | the progress service (reserved, not used yet) | none |
| `VITE_SCORES_URL` | the scores and leaderboard service (reserved, not used yet) | none |

Lists are tried in order: the first source that has an item wins, and a missing or unreachable one
falls through to the next. To prefer a backend but keep the shipped content working, list the local
registry last:

```
VITE_SCENARIO_SOURCES=https://scenarios.example.com,/registry
VITE_APP_REGISTRIES=https://apps.example.com,/registry
```

Scenario indexes from all sources are merged (an id listed twice comes from the first source).
Fetched apps are cached for the session, because references never change.

## Choosing the case

The page plays `?case={id}`, or the first case in the merged index. In `make dev` builds,
`?scenarioUrl=https://…/scenario.json` plays any scenario file. Its apps still come from the
configured registries, which is handy for trying a scenario before publishing it.

## Writing scenarios

Most scenarios are JSON only: pick published apps and configure them (see
[scenario-format.md](scenario-format.md) and [apps.md](apps.md)). The loop:

1. Put the scenario in `scenarios/{id}/scenario.json`, ideally with a `solution.json`.
2. Run `make scenario-check SCENARIO=scenarios/{id}/scenario.json` until it passes.
3. Play it with `make dev`, then open `/?case={id}`.

You only need Rust for a new kind of app (see [apps.md](apps.md#adding-a-template)).

## Verification

`replay(scenarioJson, stepsJson, apps)` re-runs a recorded session. A verifier fetches the scenario
and the exact app versions it references from the same registries, then replays the session.
