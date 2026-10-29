# Scenario format

A scenario is a folder, `scenarios/<id>/`, with `scenario.json` and the files it points to. The
JSON holds the **story and the rules** (title, briefing, hints, success, findings, which program
each image runs). Every **Kubernetes object** is written as ordinary YAML in the files listed in
`cluster.manifests`, the same documents you would `kubectl apply`. The engine reads the simulation
parts; the frontend reads the player-facing parts. Unknown top-level fields are ignored, but
everything under `success` and `findings` is validated strictly when the scenario loads, so a typo
there is an error that names the rule, not a silently ignored field.

```
scenarios/sealed-envelope/
  scenario.json
  manifests/nodes.yaml        Nodes, Pods, Deployments, Services, ... (multi-document YAML)
  manifests/payments.yaml
  case/sticky-note.txt        a file the player's terminal starts with (cat, vim, kubectl apply -f)
  solution.json               optional: one way to solve it (see "Checking a scenario")
```

## Player-facing

| field | meaning |
|---|---|
| `schemaVersion` | must be `2` (`1` listed inline nodes, pods and deployments; move them into YAML files) |
| `id`, `title` | identity and case-file title; `id` must match the folder name |
| `briefing` | the story shown in the case file |
| `objective` | the one visible goal, in the briefing's own words (no extra clues) |
| `hints` | optional list, revealed one at a time on request |

There is no time setting: **every case gets the same 15 minutes** of cluster time from the
player's start, and a case still open then closes as `timeUp` (the report names what did not hold).
A `timeLimitMinutes` field is a load error.

## The cluster

```json
"cluster": {
  "startedAt": "2026-10-27T18:24:55Z",
  "now": "2026-10-27T18:30:00Z",
  "manifests": ["manifests/nodes.yaml", "manifests/workloads.yaml"]
},
"files": ["case/sticky-note.txt"]
```

- `startedAt` / `now`: the cluster is simulated from the first to the second before the player
  arrives, so restart counts, logs, events and ages have a real history. Keep the history short:
  the player starts in the middle of the story, not at its end.
- `manifests`: YAML files, relative to `scenario.json` (no `..`, nothing absolute). They load
  through the same path as the player's `kubectl apply`, so an invalid document is a load error
  naming the file and the document. A document with a `status` is a static object (nothing
  simulates it, like a control-plane pod). Kinds: Namespace, Node, Pod, Deployment, ReplicaSet,
  Service, Endpoints, EndpointSlice, ConfigMap, Secret, NetworkPolicy, Job, CronJob, Event. The
  built-in namespaces (`default`, `kube-system`, ...) already exist.
- `files`: text files the player's terminal starts with, at the same relative paths.
- Kustomize and Helm are out of scope.

### Images

Containers run real programs: small **template apps** built to WebAssembly (see [apps.md](apps.md)
for the templates and their config). Each image picks one with `app`, a pinned `name@version`
reference like an image tag, and configures it with `config`. The app reads the container's
environment like any program, so what it logs, and whether and when it exits, follow from what the
player set. The apps themselves come from an app registry (see [registry.md](registry.md)).

```json
"imageCatalog": "closed",
"images": {
  "registry.example.com/payments-api:2.4.1": {
    "pullSeconds": 3,
    "app": "db-client@0.2.0",
    "config": { "name": "payments-api", "version": "2.4.1",
                "databaseUrlEnv": "DATABASE_URL", "portEnv": "PORT" }
  },
  "registry.example.com/badge:1.0": {
    "app": "nginx@0.1.0",
    "auth": { "username": "scanner", "password": "s3cr3t" }
  },
  "gone:1": { "exists": false }
}
```

- `pullSeconds` (default 2) is how long the first start spends pulling the image.
- `imageCatalog`: `open` (default) lets an image the scenario does not list pull fine and run a
  silent process; `closed` makes it `ErrImagePull` → `ImagePullBackOff` (a mistyped tag).
- `auth`: the image needs a docker-registry Secret with these credentials in the pod's
  `imagePullSecrets`. `exists: false` makes every pull fail with "not found".
- A reference without a version, an app the registry does not have, or any other field is an error
  when the scenario loads.

Config describes *what the app is* (its name, its port variable), never a table of outcomes: the
app's code decides what a wrong value does. Servers are found the way real ones are, through the
cluster: `db-client` logs in to whatever program listens at the URL's address (a `postgres` pod
behind a Service). Pair the scenario with a `success.assert` on state, so that the verdict is
about the configuration and not only about a pod being Ready.

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
- **`select`**: `kind` (any simulated kind: `Pod`, `Deployment`, `Service`, `Endpoints`, `Secret`,
  `Job`, ...), optional `namespace` (default `default`), `name`, and `labels` (all must match).
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

`when.command` matches the *parsed* command: `verb` (`get`, `describe`, `logs`, `exec`, `vim`,
`set-env`, `rollout-restart`, `delete`, `apply`, `create`, `replace`, `patch`, `cat`, `ls`,
`explain`, `run`, `scale`, `label`, `expose`, `set-image`, `rollout-status|history|undo`),
optional `resource` (any kubectl alias), `name` (a trailing `*` means prefix; for `cat` and `vim`
it is the file path) and, for `set-env`, `list`. A command that fails (an `error:` / `Error from
server` result) never unlocks a finding.

## The verdict

The status the judge reports has `solved`, `outcome` (`closed`, `closedWithCost` or `timeUp`),
`timeLimitSeconds` and `secondsLeft`, the findings, and a `report` once the case has closed. A case
solved in time is final; time passing afterwards changes nothing. Loading or restarting a scenario
always builds a fresh engine: no cluster, clock, file or finding carries over between attempts.

## Replay

The engine judges only from the cluster state and the parsed commands, so a recorded session
replays to the same verdict: `replay(scenarioJson, filesJson, stepsJson, apps)` takes
`[{"kind":"eval","command":"..."},{"kind":"advance","seconds":5},{"kind":"write","path":"…","text":"…"}]`
(`hint` entries are allowed and ignored). It is what `solution.json` and the engine tests use. It
is not a score-verification service; nothing here stores or ranks sessions.

## Checking a scenario

`make scenario-check` loads every `scenarios/*/scenario.json` with the locally built apps and
reports what is wrong (`SCENARIO=path/to/scenario.json` checks one file, anywhere). A
`solution.json` next to a scenario, in the replay format above, is played too, and must close the
case, and a solution should use the moves a player has (`kubectl`, `sleep`, files the scenario
ships), not engine internals:

```
ok    scenarios/crashloop-env-typo/scenario.json: loads, solution.json solves it at 21s (closed)
```

`make registry` runs the same check before it publishes the scenarios to the local registry.

A pilot scenario should also be tested against its **other valid fixes**, not only the one the
author had in mind: judge the state (assertions), never the order of commands. See
`engine/tests/pilots.rs`.
