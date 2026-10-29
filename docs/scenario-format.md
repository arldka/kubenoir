# Scenario format

A scenario is one JSON file in `scenarios/<id>/scenario.json`. The engine reads the simulation
parts; the frontend reads the player-facing parts. Unknown top-level fields are ignored, but
everything under `success` and `findings` is validated strictly when the scenario loads, so a typo
there is an error that names the rule, not a silently ignored field.

## Player-facing

| field | meaning |
|---|---|
| `schemaVersion` | must be `1` |
| `id`, `title` | identity and case-file title |
| `briefing` | the story shown in the case file |
| `objective` | the one visible goal, in the briefing's own words (no extra clues) |
| `hints` | optional list, revealed one at a time on request |

## The cluster

`cluster.startedAt` / `cluster.now` (the cluster is simulated from the first to the second, so
restart counts, logs, events and ages have a real history), `cluster.nodes`, `cluster.deployments`,
`cluster.pods` (pods that carry a `status` are static), and `images`: what each container image
runs.

### Images

Containers run real programs: small **template apps** built to WebAssembly (see [apps.md](apps.md)
for the templates and their config). Each image picks one with `app`, a pinned `name@version`
reference like an image tag, and configures it with `config`. The app reads the container's
environment like any program, so what it logs, and whether and when it exits, follow from what the
player set. The apps themselves come from an app registry (see [registry.md](registry.md)).

```json
"images": {
  "registry.example.com/payments-api:2.4.1": {
    "pullSeconds": 3,
    "app": "db-client@0.1.0",
    "config": {
      "name": "payments-api", "version": "2.4.1",
      "databaseUrlEnv": "DATABASE_URL", "portEnv": "PORT",
      "database": { "host": "postgres.default.svc.cluster.local", "port": 5432,
                    "user": "payments", "name": "payments" }
    }
  },
  "nginx:1.27": { "app": "nginx@0.1.0" }
}
```

`pullSeconds` (default 2) is how long the first start spends pulling the image. An image with no
entry, or no `app`, runs a silent process that never exits. A reference without a version, an app
the registry does not have, or any other field (including the old `rules`) is an error when the
scenario loads.

Config describes *what the app is* (its name, the database that exists for it), never a table of
outcomes: the app's code decides what a wrong value does. Pair it with a `success.assert` on the
Deployment's environment (for example `spec.template.spec.containers[0].env[name=DATABASE_URL].value`
`equals` the real value) so that the verdict is about the configuration, not only about the pod
being Ready. `db-client` accepts exactly one URL, so the two always agree.

## Success

```json
"success": {
  "stableSeconds": 15,
  "assert": [
    { "label": "payments-api is available again",
      "select": { "kind": "Deployment", "name": "payments-api" },
      "each": [ { "path": "status.availableReplicas", "equals": 1 } ] }
  ],
  "constraints": [
    { "label": "the web deployment was left alone",
      "select": { "kind": "Deployment", "name": "web" },
      "each": [ { "path": "metadata.generation", "equals": 1 } ] },
    { "label": "payments-api was not deleted",
      "forbid": { "verb": "delete", "resource": "deployments", "name": "payments-api" } }
  ]
}
```

- **Assertions** must all hold *continuously* for `stableSeconds` of cluster time (default 15), so
  a fix that flaps does not count. The case then closes automatically.
- **Constraints** are checked at every instant from the moment the player starts and are sticky:
  once broken they stay broken. The case still closes, but with the verdict `closedWithCost` and
  the report names what was broken and when. A constraint is either a state rule (`select`) or a
  `forbid`den command.
- **`select`**: `kind` (`Pod`, `Deployment`, `ReplicaSet`, `Event`), optional `namespace`
  (default `default`), `name`, and `labels` (all must match).
- **`count`**: `min`, `max` or `exactly`. Assertions default to "at least one"; constraints to no
  requirement.
- **`each`**: conditions every selected object must satisfy. `path` is dotted, with `[0]` for an
  index, `[key=value]` to pick an array element, and `["a.b/c"]` for keys containing dots, for
  example `status.conditions[type=Ready].status`. Operators (all given ones must hold): `equals`,
  `notEquals`, `exists`, `atLeast`, `atMost`, `contains`.

## Findings

Hidden clues, unlocked by investigating. The player sees only how many there are (`1 / 3`) until
each is found; the report reveals the ones they missed.

```json
"findings": [
  { "id": "logs",
    "clue": "The app's last words: 'required environment variable DATABASE_URL is not set'.",
    "when": { "command": { "verb": "logs", "name": "payments-api*" } } },
  { "id": "environment", "clue": "...",
    "when": { "anyOf": [ { "command": { "verb": "describe", "name": "payments-api*" } },
                         { "command": { "verb": "set-env", "name": "payments-api", "list": true } } ] } }
]
```

`when.command` matches the *parsed* command: `verb` (`get`, `describe`, `logs`, `set-env`,
`rollout-restart`, `delete`), optional `resource` (any kubectl alias), `name` (a trailing `*`
means prefix) and, for `set-env`, `list`. A command that fails (an `error:` / `Error from server`
result) never unlocks a finding.

## Replay and verification

The engine judges only from the cluster state and the parsed commands, so a recorded session
replays to the same verdict: `replay(scenarioJson, stepsJson, apps)` (also exposed to JavaScript) takes
`[{"kind":"eval","command":"..."},{"kind":"advance","seconds":5}]` (`hint` entries are allowed
and ignored) and returns the final status. Nothing about the verdict depends on wall-clock time.

## Checking a scenario

`make scenario-check` loads every `scenarios/*/scenario.json` with the locally built apps and
reports what is wrong (`SCENARIO=path/to/scenario.json` checks one file, anywhere). A
`solution.json` next to a scenario, in the replay format above, is played too, and must close the
case:

```
ok    scenarios/crashloop-env-typo/scenario.json: loads, solution.json solves it at 21s (closed)
```

`make registry` runs the same check before it publishes the scenarios to the local registry.
