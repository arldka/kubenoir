# SPEC — sim fidelity roadmap

Source: design review 2026-10-28 (`~/.claude/plans/do-not-edit-any-piped-willow.md`).

## §G goal

Browser-only Rust/WASM k8s sim faithful enough → scenario cluster objects authored as real k8s YAML files (referenced from scenario JSON), off-path correct fixes accepted, Service/DNS/NetworkPolicy/probe cases playable; drive via 6 pilot scenarios (#1 #3 #8 #15 #18 #20).

## §C constraints

- C1: browser-only. ⊥ real apiserver, ⊥ KWOK, ⊥ kind/vcluster/field-kit, ⊥ WebVM, ⊥ `ApiBackend` seam. kubectl layer & judge work on `ClusterStore` direct.
- C2: sim logic, cmd parsing, validation, oracle ∈ `engine/` (Rust). frontend = UI/terminal only.
- C3: replay-based score verification ⊥ goal; ⊥ design constraint from it. Runs stay seeded only so `make scenario-check` plays `solution.json` reproducibly. `replay` js fn kept as-is, ⊥ new investment.
- C4: published app `name@version` immutable; behavior change → version bump.
- C5: template config describes app; behavior ∈ app code; ⊥ `when/then` tables (incl. oracle/probe outcomes).
- C6: networking = connectivity oracle (engine fn of cluster state), ⊥ packet/socket sim, ⊥ socket host calls. Replaces old "apps mock connections" rule.
- C7: store stays one generic table keyed (kind, namespace, name) → JSON (`engine/src/store.rs`).
- C8: validation via `k8s-openapi` types (`0.28`, `v1_36`); ⊥ hand-written schema per kind.
- C9: kustomize/helm ⊥ scope (no `apply -k`, no `helm`); unsupported → V5 error.
- C10: `make ci` green ∀ task.
- C11: scenario JSON = story & rules only (`id`, `title`, `briefing`, `objective`, `hints`, `success`, `findings`, `images`, clock). ∀ k8s objects (Nodes incl.) ∈ YAML files it references; ⊥ k8s objects inline in JSON.
- C12: `kubectl edit` ⊥ scope (change state by editing a file & `kubectl apply|replace`) → the boss scolds (V28), ⊥ "not available"; terminal ships a small `vim` for the terminal's own files instead.
- C13: each scenario = one fresh simulation: load & restart build a new `Engine` from the scenario files (+ apps); ⊥ cluster, clock, files, findings or judge state carried between scenarios | attempts. Time limit = 15 min of cluster time from player start, same for every scenario (⊥ per-scenario setting); unsolved at 15 min → case closes failed.
- C14: connectivity (T20–T24) = simplest model: a connection attempt is answered from the cluster state when the container started (start-time snapshot), ⊥ resumable runs, ⊥ re-simulation for networking. A fix shows when the container next starts (crash restart / backoff / rollout); player waits. Volumes (T17) keep their own timeline.
- C15: kubelet retries are flat, ⊥ Kubernetes' doubling backoff: a container that exited restarts 10 s after it exited; a failed image pull, a failed volume mount and a missing ConfigMap/Secret key are retried every 10 s; CrashLoopBackOff message says `back-off 10s`. Warm-up history replays accordingly (a 47 min crash loop = 256 restarts).

## §I interfaces

- scenario: `scenarios/<id>/scenario.json` schemaVersion `2` → `cluster: {startedAt, now, manifests: ["manifests/nodes.yaml", "manifests/payments.yaml"]}`; paths relative to `scenario.json`, ⊥ `..`, ⊥ absolute. `cluster.nodes`/`deployments`/`pods` removed; schemaVersion `1` → load error naming migration.
- scenario: manifest YAML = real k8s docs, multi-doc (`---`); doc w/ `status` → static object (as old static pods).
- registry: publish `scenarios/{id}/scenario.json` + referenced files under `scenarios/{id}/` (replaces `scenarios/{id}.json`); client resolves refs vs scenario URL; `?scenarioUrl=` same.
- engine: load(scenario_json, files: {path → text}, apps); frontend fetches files, native `scenario-check` reads disk.
- scenario: `images.<ref>` → add `exists`(default true) & `pullSecret`(name ?) → ImagePullBackOff modeling.
- scenario: `success`/`constraints` `select.kind` → ∀ stored kind (not only `Pod`/`Deployment`/`ReplicaSet`/`Event`).
- scenario: `findings.when.command.verb` → ∀ parsed verb.
- cmd: `kubectl get|describe|logs|delete` (exist) + `apply -f`, `create <kind>|-f`, `run`, `replace --force`, `patch`, `set env|image`, `rollout status|history|undo|restart`, `scale`, `expose`, `exec`, `label`, `cordon|uncordon|drain`, `explain`, `auth can-i` ?, `--help` ∀ verb.
- cmd: flags `-n`, `-A`, `-o yaml|json|wide|name|jsonpath=`, `-l`, `-c`.
- shell: pipes `|`, builtins `echo [-n]`, `base64 [-d]`, `grep`, `cat`, `head`, `curl` (via `kubectl exec`), heredoc ? for `apply -f -`.
- fs: player virtual fs (files for `apply -f`), seeded by scenario.
- app ABI: WASI `wasm32-wasip1`; add preopened fs (ConfigMap/Secret/emptyDir/PVC mounts).
- app ABI: listen declaration (port) + request handler export → reply for curl/probes.
- app ABI: connect host call `connect(host, port)` → `NxDomain|Refused|Timeout|Connected` answered by oracle.
- js: `Engine.eval`, `advance`, `status`, `complete` (exist; keep shape); constructor gains manifest files; `replay` follows, ⊥ priority.
- shell: `vim FILE` | `vi FILE` → engine validates path (relative, safe, ⊥ dir) & queues an editor request; ⊥ in a pipeline | w/ redirect.
- js: `Engine.readFile(path)` → text|undefined; `Engine.takeEditorRequest()` → path|undefined (set by `vim`); `Engine.writeFile(path, text)` (exists).
- ui: `frontend/src/lib/terminal/vim.ts` = pure modal-editor state machine (keys in → screen model out); terminal draws it full-screen.
- docs: `docs/scenario-format.md`, `docs/apps.md`, `CLAUDE.md` container-apps bullet.

## §V invariants

- V1: ∀ create/apply/replace/patch → manifest deserializes into `k8s-openapi` type for its kind, else reject w/ kubectl-shaped error, store unchanged.
- V2: ∀ write → server-side defaults applied (e.g. `metadata.uid`, `creationTimestamp`, `generation`, Pod `restartPolicy`) before store.
- V3: Pod update touching field ∉ mutable set (`spec.containers[*].image`, `spec.initContainers[*].image`, `spec.activeDeadlineSeconds`, `spec.tolerations` additions, `spec.terminationGracePeriodSeconds`) → reject w/ real `Forbidden: pod updates may not change fields other than ...` text.
- V4: `apply` same manifest twice → 2nd no-op, `metadata.generation` unchanged, output `<kind>/<name> unchanged`.
- V5: ∀ unsupported verb|flag|kind → explicit error naming it; ⊥ silent partial execution.
- V6: error & table output strings match real kubectl for ∀ supported cmds (golden tests).
- V7: `--help` ∀ supported verb; lists only supported subcommands/flags.
- V8: oracle = pure fn(store snapshot, source pod, host, port); ⊥ randomness, ⊥ app-provided outcome tables.
- V9: oracle DNS: short `svc` → source ns; `svc.ns`, `svc.ns.svc`, `svc.ns.svc.cluster.local` resolve; ExternalName → follows target; unknown → `NxDomain`.
- V10: oracle path: Service port → targetPort (number|named) → ready endpoints matching selector → NetworkPolicy egress(src) & ingress(dst) → dst container declares listen on port. ∅ endpoints → `Refused`; policy drop → `Timeout`; no listener → `Refused`.
- V11: Service endpoints include only pods w/ `Ready=True` matching selector in Service ns.
- V12: env `valueFrom`/`envFrom` resolved at container start; later ConfigMap/Secret change ⊥ affects running container env.
- V13: mounted ConfigMap/Secret volume refreshes after kubelet sync delay; `subPath` mount ⊥ refreshes.
- V14: emptyDir data survives container restart, ⊥ survives pod replace; PVC data survives pod replace.
- V15: Secret `data` base64-decoded byte-exact (trailing `\n` kept) before env/file.
- V16: image ref ∉ catalog | missing pull secret → `ErrImagePull` → `ImagePullBackOff` w/ events; container ⊥ runs.
- V17: Job: failed pod count > `backoffLimit` → Job `Failed`; CronJob fires on cron schedule vs virtual clock, honors `concurrencyPolicy`.
- V18: probe outcome derived from app listen/handler + oracle-equivalent local check; liveness fail ×`failureThreshold` → container restart w/ event.
- V20: ∀ scenario ∈ `scenarios/` has `solution.json` closing case under `make scenario-check`.
- V21: ∀ pilot scenario → ≥1 alternate valid fix in tests accepted (off-path), where one exists.
- V22: scenario manifests load through same path as player `apply` (V1, V2); invalid doc → load error naming file & doc index.
- V23: manifest ref missing | escapes scenario dir | unreadable → load error naming ref; ⊥ partial cluster.
- V24: vim `:w`|`:wq`|`:x` → terminal file text = buffer lines joined `\n` + final `\n`; ⊥ any other file touched; ⊥ cluster change (only `kubectl apply|create|replace` reads a file).
- V25: unsaved edits + `:q` → refused w/ `E37: No write since last change (add ! to override)`; `:q!` discards; modified buffer shows `[+]`.
- V26: insert-mode Tab → 2 spaces, ⊥ `\t` (YAML); Enter, `o`, `O` keep previous line's indent.
- V27: ∀ unsupported vim key|ex command → status-line message naming it (`E492: Not an editor command: foo`); ⊥ silent ignore, ⊥ partial effect.
- V28: `kubectl edit` (any args, incl `--help`, `-n` before verb) → stderr `We don't do that here, we are not that kind of people.`; ⊥ `error:` prefix; ⊥ cluster|file change; ⊥ finding.
- V29: vim failed exit attempts per session = `:q` refused (E37) + Ctrl-C + Ctrl-Z; ≥3 → status line `I knew you were a rookie but I didn't know you were this incapable` (replaces that attempt's normal message); editor stays open; `:q!`|`:wq`|`:x` still exit.
- V30: elapsed cluster time since player start ≥ 15 min (900 s) & case unsolved → status `solved: true`, outcome `timeUp`, report lists the assertions not held; solved before 15 min ⊥ affected; scenario JSON with `timeLimitMinutes` → ⊥ ignored silently (load error: limit is fixed).
- V31: ∀ scenario load | restart → `Engine::from_scenario` anew; two engines built from same inputs ⊥ share state; after restart: cluster = scenario's, clock = start, findings 0, files = seeded, timer = 15 min.
- V32: connection outcome (T20+) = pure fn(store snapshot at container start, source pod, host, port); later cluster change ⊥ alters a running container's outcome.

## §T tasks

id|status|task|cites
T1|x|generic write path: kind registry (kind ↔ `k8s-openapi` type, scope ns\|cluster, aliases) over `ClusterStore`|C7,C8,V1,V2
T2|x|`create -f`, `apply -f` (multi-doc, file\|stdin), `delete -f`; last-applied annotation; unchanged detection|V1,V2,V4,I.cmd
T3|x|schemaVersion `2`: `cluster.manifests` refs → YAML loaded via T1 path; drop `nodes`/`deployments`/`pods`; static `status` docs|C11,I.scenario,I.engine,V22,V23
T3a|x|migrate `scenarios/crashloop-env-typo` & `sample-crashloop` to JSON + `manifests/*.yaml`; `scenario-check` reads refs from disk|C11,V20
T3b|x|registry: `make registry` publishes scenario dir + refs; `frontend/src/lib/registry.ts` fetches refs vs scenario URL; `?scenarioUrl=` resolves same|I.registry,V23
T4|x|judge: `select.kind` ∀ registered kind; `findings` ∀ verb|I.scenario
T5|x|Pod immutability rules + `replace --force` + `patch` (strategic/merge/json)|V3,V1
T6|x|player virtual fs + scenario seeding; `cat`, `ls` builtins|I.fs
T7|x|minimal shell: tokenizer w/ pipes, `echo -n`, `base64 -d`, `grep`, `head`|I.shell
T8|x|output fidelity: `-o yaml\|json\|name\|wide\|jsonpath`, `-l`, `-A`, golden tests vs real kubectl captures|V6,I.cmd
T9|x|`--help` ∀ verb + `explain` from `k8s-openapi` docs|V7
T10|x|unsupported verb/flag/kind errors audit|V5
T35|x|engine: `vim`\|`vi` FILE command (parse, path rules, queue editor request), `Engine.readFile`, `takeEditorRequest`; judge verb `vim` (name = path); help page|C12,V5,V27,I.shell,I.js
T36|x|`frontend/src/lib/terminal/vim.ts`: modal editor state machine, vitest-covered (incl. failed-exit counter): normal/insert/ex modes; h j k l w b e 0 $ gg G; i a I A o O; x dd yy p P u Ctrl-R; counts; `:w :q :wq :x :q!`; Tab→spaces, auto-indent|V24,V25,V26,V27,V29,I.ui
T37|x|terminal integration: full-screen render (alt screen, mode/`[+]` status line, `~` rows), key routing in `session.ts`, `:w` → `Engine.writeFile`, back to prompt on quit; e2e: edit a manifest then `kubectl apply -f` it; fail to quit 3× → scolded, then `:q!` leaves|V24,V25,V29,I.js
T38|x|`kubectl edit` → boss's reprimand (parse error variant printed as is, stderr in pipelines)|C12,V28
T39|.|time limit: fixed 15 min → judge closes unsolved case at 900 s as `timeUp` (report: what did not hold); status exposes limit & seconds left; UI timer counts down; restart builds a fresh engine (timer, cluster, files, findings reset); engine + e2e tests|C13,V30,V31
T13|x|verbs: `run`, `create namespace\|deployment\|configmap\|secret`, `set image`, `scale`, `rollout status\|history\|undo`, `label`, `expose`|I.cmd
T14|x|Namespace kind + cluster-scoped resources + `namespaces "x" not found`|V1
T15|x|env `valueFrom`/`envFrom` from ConfigMap/Secret at start|V12,V15
T16|x|image catalog + `imagePullSecrets` → ErrImagePull/ImagePullBackOff|V16,I.scenario
T17|x|WASI fs preopens in `engine/src/apps/wasi.rs`: ConfigMap/Secret/emptyDir mounts, read-only where k8s is|I.app,V13,V14,C4
T18|x|decided (C14): oracle answers from the cluster state at container start; ⊥ resumable runs; fix visible after the container restarts|C3,C6,C14,V32
T19|x|Service + EndpointSlice controller|V11
T20|x|connectivity oracle in `engine/src/` (DNS, ports, endpoints, listen check), evaluated at container start|V8,V9,V10,V32,C6,C14
T21|x|app ABI: listen declaration + request handler; `kit` helpers; new `http-responder` template app|I.app,C4,C5
T22|x|`kubectl exec <pod> -- curl` builtin via oracle + handler|I.shell,V10
T23|x|`db-client` new version: connect via oracle host call, server described by target pod's app (`postgres` template), ⊥ static `database.host`|C4,C5,C6
T24|x|NetworkPolicy eval in oracle|V10
T25|x|probes (http/tcp/exec) + liveness restarts + readiness → endpoints|V18,V11
T26|x|pilot #1 New Badge (namespace)|T9,T14,V20
T27|x|pilot #3 Changing the Story (pod immutability; `echo-env` app; player edits the pod manifest with vim, then `replace --force`)|T5,T35,T36,T37,V3,V20
T28|x|pilot #8 Sealed Envelope (secret base64 newline)|T7,T15,V15,V20,V21
T29|x|pilot #15 Wrong Mugshot (ImagePullBackOff)|T16,V16,V20
T30|x|pilot #18 Nobody's Answering (selector mismatch)|T19,T20,T22,V20,V21
T31|x|pilot #20 Out of Jurisdiction (cross-ns DNS; FQDN \| ExternalName both pass)|T20,T23,V9,V21
T32|x|Job + CronJob controllers, cron parser, time-skip|V17
T34|.|docs: update `docs/scenario-format.md` (schemaVersion `2`, manifests), `docs/registry.md` (dir layout, replay ⊥ score verification), `docs/apps.md`, `CLAUDE.md` networking & scoring bullets|C3,C6,C11,I.docs

## §B bugs

id|date|cause|fix
