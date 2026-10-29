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

## §I interfaces

- scenario: `scenarios/<id>/scenario.json` schemaVersion `2` → `cluster: {startedAt, now, manifests: ["manifests/nodes.yaml", "manifests/payments.yaml"]}`; paths relative to `scenario.json`, ⊥ `..`, ⊥ absolute. `cluster.nodes`/`deployments`/`pods` removed; schemaVersion `1` → load error naming migration.
- scenario: manifest YAML = real k8s docs, multi-doc (`---`); doc w/ `status` → static object (as old static pods).
- registry: publish `scenarios/{id}/scenario.json` + referenced files under `scenarios/{id}/` (replaces `scenarios/{id}.json`); client resolves refs vs scenario URL; `?scenarioUrl=` same.
- engine: load(scenario_json, files: {path → text}, apps); frontend fetches files, native `scenario-check` reads disk.
- scenario: `images.<ref>` → add `exists`(default true) & `pullSecret`(name ?) → ImagePullBackOff modeling.
- scenario: `success`/`constraints` `select.kind` → ∀ stored kind (not only `Pod`/`Deployment`/`ReplicaSet`/`Event`).
- scenario: `findings.when.command.verb` → ∀ parsed verb.
- cmd: `kubectl get|describe|logs|delete` (exist) + `apply -f`, `create <kind>|-f`, `run`, `edit`, `replace --force`, `patch`, `set env|image`, `rollout status|history|undo|restart`, `scale`, `expose`, `exec`, `label`, `cordon|uncordon|drain`, `explain`, `auth can-i` ?, `--help` ∀ verb.
- cmd: flags `-n`, `-A`, `-o yaml|json|wide|name|jsonpath=`, `-w`, `-l`, `-c`.
- shell: pipes `|`, builtins `echo [-n]`, `base64 [-d]`, `grep`, `cat`, `head`, `curl` (via `kubectl exec`), heredoc ? for `apply -f -`.
- fs: player virtual fs (files for `apply -f`), seeded by scenario.
- app ABI: WASI `wasm32-wasip1`; add preopened fs (ConfigMap/Secret/emptyDir/PVC mounts).
- app ABI: listen declaration (port) + request handler export → reply for curl/probes.
- app ABI: connect host call `connect(host, port)` → `NxDomain|Refused|Timeout|Connected` answered by oracle.
- js: `Engine.eval`, `advance`, `status`, `complete` (exist; keep shape); constructor gains manifest files; `replay` follows, ⊥ priority.
- js: editor hook for `kubectl edit` (frontend opens buffer, returns text to engine).
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
- V19: `kubectl edit` → saved buffer goes through same path as `replace` (V1–V3); unchanged buffer → `Edit cancelled, no changes made.`
- V20: ∀ scenario ∈ `scenarios/` has `solution.json` closing case under `make scenario-check`.
- V21: ∀ pilot scenario → ≥1 alternate valid fix in tests accepted (off-path), where one exists.
- V22: scenario manifests load through same path as player `apply` (V1, V2); invalid doc → load error naming file & doc index.
- V23: manifest ref missing | escapes scenario dir | unreadable → load error naming ref; ⊥ partial cluster.

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
T9|.|`--help` ∀ verb + `explain` from `k8s-openapi` docs|V7
T10|.|unsupported verb/flag/kind errors audit|V5
T11|.|`get -w` streaming in terminal (engine emits watch events on `advance`)|I.js
T12|.|`kubectl edit` frontend buffer + engine replace path|V19,I.js
T13|.|verbs: `run`, `create namespace\|deployment\|configmap\|secret`, `set image`, `scale`, `rollout status\|history\|undo`, `label`, `expose`|I.cmd
T14|.|Namespace kind + cluster-scoped resources + `namespaces "x" not found`|V1
T15|.|env `valueFrom`/`envFrom` from ConfigMap/Secret at start|V12,V15
T16|.|image catalog + `imagePullSecrets` → ErrImagePull/ImagePullBackOff|V16,I.scenario
T17|.|WASI fs preopens in `engine/src/apps/wasi.rs`: ConfigMap/Secret/emptyDir mounts, read-only where k8s is|I.app,V13,V14,C4
T18|.|decide oracle timing: answer from start-time snapshot \| resumable app runs (wasmi resumable calls); replay ⊥ constraint (C3). record in §C|C3,C6,V8
T19|.|Service + EndpointSlice controller|V11
T20|.|connectivity oracle in `engine/src/` (DNS, ports, endpoints, listen check)|V8,V9,V10,C6
T21|.|app ABI: listen declaration + request handler; `kit` helpers; new `http-responder` template app|I.app,C4,C5
T22|.|`kubectl exec <pod> -- curl` builtin via oracle + handler|I.shell,V10
T23|.|`db-client` new version: connect via oracle host call, server described by target pod's app (`postgres` template), ⊥ static `database.host`|C4,C5,C6
T24|.|NetworkPolicy eval in oracle|V10
T25|.|probes (http/tcp/exec) + liveness restarts + readiness → endpoints|V18,V11
T26|.|pilot #1 New Badge (namespace)|T9,T14,V20
T27|.|pilot #3 Changing the Story (pod immutability; `echo-env` app)|T5,T12,V3,V20
T28|.|pilot #8 Sealed Envelope (secret base64 newline)|T7,T15,V15,V20,V21
T29|.|pilot #15 Wrong Mugshot (ImagePullBackOff)|T16,V16,V20
T30|.|pilot #18 Nobody's Answering (selector mismatch)|T19,T20,T22,V20,V21
T31|.|pilot #20 Out of Jurisdiction (cross-ns DNS; FQDN \| ExternalName both pass)|T20,T23,V9,V21
T32|.|Job + CronJob controllers, cron parser, time-skip|V17
T34|.|docs: update `docs/scenario-format.md` (schemaVersion `2`, manifests), `docs/registry.md` (dir layout, replay ⊥ score verification), `docs/apps.md`, `CLAUDE.md` networking & scoring bullets|C3,C6,C11,I.docs

## §B bugs

id|date|cause|fix
