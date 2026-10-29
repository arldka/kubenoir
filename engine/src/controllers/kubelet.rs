//! The kubelet: runs each scheduled pod's containers through their lifecycle.
//!
//! Per container:
//! ```text
//! Creating --(image pull)--> Running --(program exits)--> BackOff --(delay)--> Running ...
//!                                    \-(policy says stop)--> Exited
//! ```
//! A container that exited, an image that could not be pulled, a volume that could not be mounted
//! and a missing ConfigMap are all retried every 10 s, flat: this game does not use the real
//! kubelet's doubling backoff (10 s up to 300 s), so a fix shows within seconds. Every transition is stamped with the time it was due, appends log lines and
//! events, and is mirrored into `pod.status`. Only pods the scheduler admitted (status phase
//! `Pending` with a node) are simulated; pods authored with a status stay static.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Container, ContainerState, ContainerStateRunning, ContainerStateTerminated, ContainerStateWaiting, ContainerStatus,
    Pod, PodCondition, PodSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::volumes::{self, VolumeState};
use super::{container_env, images, pod_identity, pods, Env};
use crate::apps::{self, Launch, Mount, Program, VolEvent};
use crate::clock;
use crate::events::{self, EventType, Involved, Source};
use crate::names;
use crate::store::StoreError;

/// Runtime-row name for pod-level state (as opposed to a container's).
const POD_ROW: &str = "*";
/// How long the kubelet waits before it tries again: restarting an exited container, pulling an
/// image again, mounting volumes, creating a container whose ConfigMap is missing.
const RETRY_SECS: i64 = 10;
/// A container counts as Ready once it has been running this long. Combined with exits winning
/// ties, a process that dies within a second never flaps Ready, so rollouts of a crashing
/// image stall exactly as they do on a real cluster.
const READY_DELAY_SECS: i64 = 1;
/// A failed pull shows as `ErrImagePull` for this long, then as `ImagePullBackOff` until the
/// next attempt.
const ERR_IMAGE_PULL_SECS: i64 = 5;
/// Safety valve against a program that could never advance time.
const MAX_STEPS_PER_PASS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Creating,
    Running,
    BackOff,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Terminated {
    exit_code: i32,
    reason: String,
    started_at: i64,
    finished_at: i64,
}

/// Persisted per-container kubelet state (`runtime` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Runtime {
    phase: Phase,
    /// Which run of the container this is; 0 for the first start.
    instance: u32,
    restart_count: i32,
    started_at: i64,
    /// `Creating`: when the image is pulled and the container starts. `BackOff`: when it restarts.
    due: i64,
    backoff_secs: i64,
    program: Program,
    lines_emitted: usize,
    #[serde(default)]
    ready: bool,
    last: Option<Terminated>,
    /// Why the container cannot be created yet (a ConfigMap or Secret it refers to is missing);
    /// the kubelet tries again every [`RETRY_SECS`].
    #[serde(default)]
    config_error: Option<String>,
    /// Why the image could not be pulled on the last attempt; the container is waiting to try
    /// again (backoff doubling per failure, like the real kubelet's image pull backoff).
    #[serde(default)]
    pull_error: Option<String>,
    #[serde(default)]
    pull_failures: u32,
    #[serde(default)]
    pull_failed_at: i64,
    /// What the current run was started with, kept so the run can be simulated again when its
    /// volumes change.
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    mounts: Vec<Mount>,
    /// The pod's volumes could not be mounted yet, so nothing else has started for this
    /// container: no image pull, no start.
    #[serde(default)]
    awaiting_volumes: bool,
}

enum Step {
    Start,
    Line(usize),
    Exit,
    Ready,
}

impl Runtime {
    fn creating(due: i64) -> Self {
        Runtime {
            phase: Phase::Creating,
            instance: 0,
            restart_count: 0,
            started_at: 0,
            due,
            backoff_secs: 0,
            program: Program::default(),
            lines_emitted: 0,
            ready: false,
            last: None,
            config_error: None,
            pull_error: None,
            pull_failures: 0,
            pull_failed_at: 0,
            env: BTreeMap::new(),
            mounts: Vec::new(),
            awaiting_volumes: false,
        }
    }

    /// The earliest thing this container will do next, and when.
    fn next_step(&self) -> Option<(i64, Step)> {
        match self.phase {
            Phase::Creating | Phase::BackOff => Some((self.due, Step::Start)),
            Phase::Exited => None,
            Phase::Running => {
                // At equal times: log lines first, then an exit, then readiness.
                let mut best: Option<(i64, u8, Step)> = None;
                let mut consider = |candidate: (i64, u8, Step)| {
                    if best.as_ref().is_none_or(|b| (candidate.0, candidate.1) < (b.0, b.1)) {
                        best = Some(candidate);
                    }
                };
                if let Some((at, _)) = self.program.lines.get(self.lines_emitted) {
                    consider((self.started_at + at, 0, Step::Line(self.lines_emitted)));
                }
                if let Some((at, _)) = self.program.exit {
                    consider((self.started_at + at, 1, Step::Exit));
                }
                if !self.ready {
                    consider((self.started_at + READY_DELAY_SECS, 2, Step::Ready));
                }
                best.map(|(at, _, step)| (at, step))
            }
        }
    }
}

struct PodCtx<'a> {
    env: &'a Env<'a>,
    /// The pod as it was when this pass began (its fields feed `fieldRef` variables).
    pod: Pod,
    namespace: String,
    name: String,
    uid: String,
    node: String,
    restart_policy: String,
}

impl PodCtx<'_> {
    fn event(
        &self,
        container: Option<&str>,
        event_type: EventType,
        reason: &str,
        message: &str,
        at: i64,
    ) -> Result<(), StoreError> {
        let involved = Involved {
            kind: "Pod",
            api_version: "v1",
            namespace: &self.namespace,
            name: &self.name,
            uid: &self.uid,
            field_path: container.map(|c| format!("spec.containers{{{c}}}")),
        };
        let source = Source {
            component: "kubelet",
            host: Some(&self.node),
        };
        events::record(self.env.store, &involved, event_type, reason, message, &source, at)
    }
}

/// Which run of a container `kubectl logs` should read.
#[derive(Debug, PartialEq, Eq)]
pub enum LogSelection {
    Instance(u32),
    /// The container has not started yet.
    WaitingToStart,
    /// `--previous` was asked for but the container has only run once.
    NoPrevious,
}

/// Mirrors kubectl: without `--previous` you get the latest run (still running, or the last one
/// that exited while the container waits in backoff); with it, the run before the live one.
/// `None` when the container has no kubelet state (a static pod).
pub fn select_log_instance(
    store: &crate::store::ClusterStore,
    namespace: &str,
    pod: &str,
    container: &str,
    previous: bool,
) -> Result<Option<LogSelection>, StoreError> {
    let Some(state) = store.runtime_get(namespace, pod, container)? else {
        return Ok(None);
    };
    let rt: Runtime = serde_json::from_value(state)?;
    Ok(Some(match (rt.phase, previous) {
        (Phase::Creating, _) => LogSelection::WaitingToStart,
        (Phase::Running, true) if rt.instance == 0 => LogSelection::NoPrevious,
        (Phase::Running, true) => LogSelection::Instance(rt.instance - 1),
        (Phase::Running, false) | (Phase::BackOff | Phase::Exited, _) => LogSelection::Instance(rt.instance),
    }))
}

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut changed = false;
    for pod in pods(env.store)? {
        changed |= step_pod(env, pod)?;
    }
    Ok(changed)
}

fn step_pod(env: &Env<'_>, mut pod: Pod) -> Result<bool, StoreError> {
    let before = serde_json::to_value(&pod)?;
    let (namespace, name, uid) = pod_identity(&pod);

    let rows = env.store.runtime_for_pod(&namespace, &name)?;
    let has_pod_row = rows.iter().any(|(c, _)| c == POD_ROW);
    let mut runtimes: BTreeMap<String, Runtime> = BTreeMap::new();
    let mut vols = VolumeState::default();
    for (container, state) in rows {
        if container == volumes::ROW {
            vols = serde_json::from_value(state)?;
        } else if container != POD_ROW {
            runtimes.insert(container, serde_json::from_value(state)?);
        }
    }
    let vols_before = vols.clone();

    // A deleted pod stops evolving and disappears when its grace period ends.
    if let Some(deleted_at) = pod.metadata.deletion_timestamp.as_ref().map(clock::secs) {
        if env.now >= deleted_at {
            env.store.delete("Pod", &namespace, &name)?;
            env.store.purge_pod_state(&namespace, &name)?;
            return Ok(true);
        }
        if has_pod_row {
            return Ok(false);
        }
        for (container, rt) in &runtimes {
            env.store
                .runtime_put(&namespace, &name, container, &serde_json::to_value(rt)?, None)?;
        }
        env.store.runtime_put(
            &namespace,
            &name,
            POD_ROW,
            &json!({ "deleteAt": deleted_at }),
            Some(deleted_at),
        )?;
        return Ok(true);
    }

    let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
    let node = pod.spec.as_ref().and_then(|s| s.node_name.clone());
    let admit = runtimes.is_empty() && phase == Some("Pending") && node.is_some();
    if runtimes.is_empty() && !admit {
        return Ok(false); // static pod
    }
    let (Some(spec), Some(node)) = (pod.spec.clone(), node) else {
        return Ok(false);
    };

    let ctx = PodCtx {
        env,
        pod: pod.clone(),
        namespace: namespace.clone(),
        name: name.clone(),
        uid,
        node,
        restart_policy: spec.restart_policy.clone().unwrap_or_else(|| "Always".into()),
    };

    let mut did = false;
    if admit {
        // Volumes are mounted first; only then does the kubelet pull images and start containers.
        let mount_error = vols.initialize(env.store, &pod)?.err();
        if let Some(message) = &mount_error {
            ctx.event(None, EventType::Warning, "FailedMount", message, env.now)?;
        }
        for container in &spec.containers {
            let image = container.image.clone().unwrap_or_default();
            if mount_error.is_some() {
                let mut waiting = Runtime::creating(env.now + RETRY_SECS);
                waiting.awaiting_volumes = true;
                runtimes.insert(container.name.clone(), waiting);
                continue;
            }
            ctx.event(
                Some(&container.name),
                EventType::Normal,
                "Pulling",
                &format!("Pulling image \"{image}\""),
                env.now,
            )?;
            runtimes.insert(
                container.name.clone(),
                Runtime::creating(env.now + env.config.pull_seconds(&image)),
            );
        }
        did = true;
    }

    // ConfigMaps and Secrets mounted as volumes may have changed since the volume matched them.
    let mut dirty = vols.ready && vols.observe(env.store, &pod, env.now)?;
    for container in &spec.containers {
        let peers = peer_writes(&runtimes, &container.name);
        let Some(rt) = runtimes.get_mut(&container.name) else {
            continue;
        };
        for _ in 0..MAX_STEPS_PER_PASS {
            let Some((at, step)) = rt.next_step() else { break };
            if at > env.now {
                break;
            }
            let started = matches!(step, Step::Start);
            apply(&ctx, container, rt, at, step, &mut vols, &peers)?;
            dirty |= started;
            did = true;
        }
    }
    // A new run or a refreshed volume changes what the other running containers will read.
    if dirty && vols.ready {
        did |= settle(&ctx, &spec, &mut runtimes, &vols)?;
    }
    if vols != vols_before {
        env.store
            .runtime_put(&namespace, &name, volumes::ROW, &serde_json::to_value(&vols)?, None)?;
        did = true;
    }

    if did {
        for (container, rt) in &runtimes {
            let wake = rt.next_step().map(|(at, _)| at);
            env.store
                .runtime_put(&namespace, &name, container, &serde_json::to_value(rt)?, wake)?;
        }
    }

    sync_status(&mut pod, &spec, &runtimes, env.now);
    if serde_json::to_value(&pod)? != before {
        env.store
            .update("Pod", &namespace, &name, &serde_json::to_value(&pod)?)?;
        did = true;
    }
    Ok(did)
}

/// Apply one due transition of a container, at the time `t` it was due.
fn apply(
    ctx: &PodCtx<'_>,
    container: &Container,
    rt: &mut Runtime,
    t: i64,
    step: Step,
    vols: &mut VolumeState,
    peers: &[VolEvent],
) -> Result<(), StoreError> {
    let image = container.image.clone().unwrap_or_default();
    match step {
        Step::Start => {
            let first_start = rt.phase == Phase::Creating;
            if rt.awaiting_volumes {
                if !vols.ready {
                    if let Err(message) = vols.initialize(ctx.env.store, &ctx.pod)? {
                        ctx.event(None, EventType::Warning, "FailedMount", &message, t)?;
                        rt.due = t + RETRY_SECS;
                        return Ok(());
                    }
                }
                // Mounted at last: now the image is pulled, and then the container starts.
                rt.awaiting_volumes = false;
                let pulling = format!("Pulling image \"{image}\"");
                ctx.event(Some(&container.name), EventType::Normal, "Pulling", &pulling, t)?;
                rt.due = t + ctx.env.config.pull_seconds(&image);
                return Ok(());
            }
            if first_start {
                // The image comes first. Every attempt uses the pod's current image and pull
                // secrets, so fixing either takes effect on the next retry.
                let c = Some(container.name.as_str());
                if rt.pull_failures > 0 {
                    ctx.event(
                        c,
                        EventType::Normal,
                        "Pulling",
                        &format!("Pulling image \"{image}\""),
                        t,
                    )?;
                }
                if let Err(message) = images::check_pull(ctx.env, &ctx.pod, &image)? {
                    ctx.event(c, EventType::Warning, "Failed", &message, t)?;
                    ctx.event(c, EventType::Warning, "Failed", "Error: ErrImagePull", t)?;
                    let backoff = format!("Back-off pulling image \"{image}\"");
                    ctx.event(c, EventType::Normal, "BackOff", &backoff, t)?;
                    ctx.event(c, EventType::Warning, "Failed", "Error: ImagePullBackOff", t)?;
                    rt.pull_failures += 1;
                    rt.pull_error = Some(message);
                    rt.pull_failed_at = t;
                    rt.due = t + RETRY_SECS;
                    return Ok(());
                }
                rt.pull_error = None;
            }
            // The environment is read now, at the start; a running container keeps what it got.
            let resolved = match container_env::resolve(ctx.env.store, &ctx.pod, container)? {
                Ok(resolved) => resolved,
                Err(message) => {
                    let c = Some(container.name.as_str());
                    if first_start && rt.config_error.is_none() {
                        let pulled = format!(
                            "Successfully pulled image \"{image}\" in {}s",
                            ctx.env.config.pull_seconds(&image)
                        );
                        ctx.event(c, EventType::Normal, "Pulled", &pulled, t)?;
                    }
                    ctx.event(c, EventType::Warning, "Failed", &format!("Error: {message}"), t)?;
                    rt.config_error = Some(message);
                    rt.due = t + RETRY_SECS;
                    return Ok(());
                }
            };
            rt.config_error = None;
            for warning in &resolved.warnings {
                ctx.event(
                    Some(&container.name),
                    EventType::Warning,
                    "InvalidEnvironmentVariableNames",
                    warning,
                    t,
                )?;
            }
            if first_start {
                rt.instance = 0;
            } else {
                rt.instance += 1;
                rt.restart_count += 1;
            }
            let mounts = volumes::mounts_for(&ctx.pod, container);
            if !first_start {
                // What the earlier run wrote stays in the volumes for this one.
                vols.events.append(&mut rt.program.writes);
            }
            let timeline = vols.timeline(peers);
            rt.program = apps::run(
                &ctx.env.config.apps,
                ctx.env.config.images.get(&image),
                &Launch {
                    image: &image,
                    env: &resolved.env,
                    started_at: t,
                    seed: seed_of(ctx, &container.name, rt.instance),
                    mounts: &mounts,
                    timeline: &timeline,
                },
            );
            rt.env = resolved.env.clone();
            rt.mounts = mounts;
            rt.phase = Phase::Running;
            rt.started_at = t;
            rt.lines_emitted = 0;
            rt.ready = false;

            if !first_start {
                // One BackOff per restart attempt, so its count always equals RESTARTS.
                let message = format!(
                    "Back-off restarting failed container {} in pod {}_{}({})",
                    container.name, ctx.name, ctx.namespace, ctx.uid
                );
                ctx.event(Some(&container.name), EventType::Warning, "BackOff", &message, t)?;
            }

            let pulled = if first_start {
                format!(
                    "Successfully pulled image \"{image}\" in {}s",
                    ctx.env.config.pull_seconds(&image)
                )
            } else {
                format!("Container image \"{image}\" already present on machine")
            };
            let c = Some(container.name.as_str());
            ctx.event(c, EventType::Normal, "Pulled", &pulled, t)?;
            ctx.event(
                c,
                EventType::Normal,
                "Created",
                &format!("Created container {}", container.name),
                t,
            )?;
            ctx.event(
                c,
                EventType::Normal,
                "Started",
                &format!("Started container {}", container.name),
                t,
            )?;
        }
        Step::Line(index) => {
            let (at, text) = rt.program.lines[index].clone();
            ctx.env.store.append_log(
                &ctx.namespace,
                &ctx.name,
                &container.name,
                rt.instance,
                rt.started_at + at,
                &text,
            )?;
            rt.lines_emitted += 1;
        }
        Step::Ready => rt.ready = true,
        Step::Exit => {
            rt.ready = false;
            let code = rt.program.exit.map_or(0, |(_, code)| code);
            rt.last = Some(Terminated {
                exit_code: code,
                reason: if code == 0 { "Completed" } else { "Error" }.into(),
                started_at: rt.started_at,
                finished_at: t,
            });
            let restart = match ctx.restart_policy.as_str() {
                "Never" => false,
                "OnFailure" => code != 0,
                _ => true,
            };
            if !restart {
                rt.phase = Phase::Exited;
                return Ok(());
            }
            let delay = RETRY_SECS;
            rt.phase = Phase::BackOff;
            rt.due = t + delay;
            rt.backoff_secs = delay;
        }
    }
    Ok(())
}

/// Seeds a container's run: the same for the same pod, container and restart.
fn seed_of(ctx: &PodCtx<'_>, container: &str, instance: u32) -> u64 {
    names::fnv1a(format!("{}/{container}/{instance}", ctx.uid).as_bytes())
}

/// The files the pod's other containers write, which this one can read.
fn peer_writes(runtimes: &BTreeMap<String, Runtime>, except: &str) -> Vec<VolEvent> {
    runtimes
        .iter()
        .filter(|(name, _)| name.as_str() != except)
        .flat_map(|(_, rt)| rt.program.writes.iter().cloned())
        .collect()
}

/// Simulate the running containers that use volumes again, now that the volumes (or what their
/// siblings write) are known to change, and swap the new runs in. Output before now cannot
/// differ, since nothing that changes happens before now, so what was already logged stands.
/// Containers read each other's writes, so this repeats until nobody's writes change.
fn settle(
    ctx: &PodCtx<'_>,
    spec: &PodSpec,
    runtimes: &mut BTreeMap<String, Runtime>,
    vols: &VolumeState,
) -> Result<bool, StoreError> {
    let mut any = false;
    for _ in 0..4 {
        let mut writes_changed = false;
        for container in &spec.containers {
            let Some(rt) = runtimes.get(&container.name) else {
                continue;
            };
            if rt.phase != Phase::Running || rt.mounts.is_empty() {
                continue;
            }
            let image = container.image.clone().unwrap_or_default();
            let timeline = vols.timeline(&peer_writes(runtimes, &container.name));
            let program = apps::run(
                &ctx.env.config.apps,
                ctx.env.config.images.get(&image),
                &Launch {
                    image: &image,
                    env: &rt.env,
                    started_at: rt.started_at,
                    seed: seed_of(ctx, &container.name, rt.instance),
                    mounts: &rt.mounts,
                    timeline: &timeline,
                },
            );
            if program != rt.program {
                let rt = runtimes.get_mut(&container.name).expect("just read");
                writes_changed |= program.writes != rt.program.writes;
                rt.program = program;
                any = true;
            }
        }
        if !writes_changed {
            break;
        }
    }
    Ok(any)
}

/// Mirror runtime state into `pod.status`.
fn sync_status(pod: &mut Pod, spec: &PodSpec, runtimes: &BTreeMap<String, Runtime>, now: i64) {
    let Some(mut status) = pod.status.take() else { return };
    let (namespace, name, uid) = pod_identity(pod);

    let statuses: Vec<ContainerStatus> = spec
        .containers
        .iter()
        .map(|c| container_status(c, runtimes.get(&c.name), &namespace, &name, &uid, now))
        .collect();

    let all_exited = !runtimes.is_empty() && runtimes.values().all(|rt| rt.phase == Phase::Exited);
    let all_creating = runtimes.values().all(|rt| rt.phase == Phase::Creating);
    status.phase = Some(
        if all_exited {
            if runtimes
                .values()
                .all(|rt| rt.last.as_ref().is_some_and(|l| l.exit_code == 0))
            {
                "Succeeded"
            } else {
                "Failed"
            }
        } else if all_creating {
            "Pending"
        } else {
            "Running"
        }
        .into(),
    );

    let not_ready: Vec<&str> = statuses.iter().filter(|s| !s.ready).map(|s| s.name.as_str()).collect();
    let ready = not_ready.is_empty();
    let unready_msg = (!ready).then(|| format!("containers with unready status: [{}]", not_ready.join(" ")));
    let conditions = status.conditions.take().unwrap_or_default();
    let mut updated = Vec::new();
    let kept = |ty: &str| conditions.iter().find(|c| c.type_ == ty).cloned();
    let mut upsert = |ty: &str, ok: bool, reason: Option<&str>, message: Option<String>| {
        let want = if ok { "True" } else { "False" };
        let time = match kept(ty) {
            Some(old) if old.status == want => old.last_transition_time,
            _ => Some(clock::to_time(now)),
        };
        updated.push(PodCondition {
            type_: ty.into(),
            status: want.into(),
            reason: reason.map(String::from),
            message,
            last_transition_time: time,
            ..Default::default()
        });
    };
    upsert("PodReadyToStartContainers", true, None, None);
    upsert("Initialized", true, None, None);
    upsert(
        "Ready",
        ready,
        (!ready).then_some("ContainersNotReady"),
        unready_msg.clone(),
    );
    upsert(
        "ContainersReady",
        ready,
        (!ready).then_some("ContainersNotReady"),
        unready_msg,
    );
    if let Some(scheduled) = kept("PodScheduled") {
        updated.push(scheduled);
    }

    status.conditions = Some(updated);
    status.container_statuses = Some(statuses);
    status.start_time.get_or_insert_with(|| clock::to_time(now));
    pod.status = Some(status);
}

fn container_status(
    c: &Container,
    rt: Option<&Runtime>,
    namespace: &str,
    pod: &str,
    uid: &str,
    now: i64,
) -> ContainerStatus {
    let image = c.image.clone().unwrap_or_default();
    let mut status = ContainerStatus {
        name: c.name.clone(),
        image: image.clone(),
        image_id: format!("{image}@sha256:{}", digest(&image)),
        ..Default::default()
    };
    let Some(rt) = rt else {
        status.state = Some(waiting("ContainerCreating", None));
        return status;
    };

    status.restart_count = rt.restart_count;
    let terminated = |t: &Terminated| ContainerState {
        terminated: Some(ContainerStateTerminated {
            exit_code: t.exit_code,
            reason: Some(t.reason.clone()),
            started_at: Some(clock::to_time(t.started_at)),
            finished_at: Some(clock::to_time(t.finished_at)),
            container_id: Some(container_id(uid, &c.name, rt.instance)),
            ..Default::default()
        }),
        ..Default::default()
    };
    if let Some(message) = rt.pull_error.as_ref().filter(|_| rt.phase == Phase::Creating) {
        status.state = Some(if now - rt.pull_failed_at < ERR_IMAGE_PULL_SECS {
            waiting("ErrImagePull", Some(message.clone()))
        } else {
            waiting("ImagePullBackOff", Some(format!("Back-off pulling image \"{image}\"")))
        });
        return status;
    }
    if let Some(message) = rt.config_error.as_ref().filter(|_| rt.phase != Phase::Running) {
        status.state = Some(waiting("CreateContainerConfigError", Some(message.clone())));
        status.last_state = rt.last.as_ref().map(terminated);
        return status;
    }
    match rt.phase {
        Phase::Creating => status.state = Some(waiting("ContainerCreating", None)),
        Phase::Running => {
            status.ready = rt.ready;
            status.started = Some(true);
            status.state = Some(ContainerState {
                running: Some(ContainerStateRunning {
                    started_at: Some(clock::to_time(rt.started_at)),
                }),
                ..Default::default()
            });
            status.last_state = rt.last.as_ref().map(terminated);
        }
        Phase::BackOff => {
            let message = format!(
                "back-off {} restarting failed container={} pod={}_{}({})",
                go_duration(rt.backoff_secs),
                c.name,
                pod,
                namespace,
                uid
            );
            status.state = Some(waiting("CrashLoopBackOff", Some(message)));
            status.last_state = rt.last.as_ref().map(terminated);
        }
        Phase::Exited => status.state = rt.last.as_ref().map(terminated),
    }
    if rt.phase != Phase::Creating {
        status.container_id = Some(container_id(uid, &c.name, rt.instance));
    }
    status
}

fn waiting(reason: &str, message: Option<String>) -> ContainerState {
    ContainerState {
        waiting: Some(ContainerStateWaiting {
            reason: Some(reason.into()),
            message,
        }),
        ..Default::default()
    }
}

/// 64 hex chars derived from `seed`, shaped like a sha256 digest.
fn digest(seed: &str) -> String {
    (0..4)
        .map(|i| format!("{:016x}", names::fnv1a(format!("{seed}/{i}").as_bytes())))
        .collect()
}

fn container_id(uid: &str, container: &str, instance: u32) -> String {
    format!("containerd://{}", digest(&format!("{uid}/{container}/{instance}")))
}

/// Go's `time.Duration` formatting for whole seconds (`10s`, `1m20s`, `5m0s`).
fn go_duration(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m{s}s"),
        _ => format!("{h}h{m}m{s}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Engine;

    /// One status-less pod running `app:1` (the `db-client` app), which crashes one second after
    /// starting unless `DATABASE_URL` points at its database. Pulling takes the default 2 s.
    /// Start time is 2026-10-27T18:30:00Z.
    fn scenario(env: &str) -> String {
        format!(
            r#"{{
            "id": "t",
            "images": {{"app:1": {{"app": "db-client@0.1.0", "config": {{"name": "app", "version": "1",
                "database": {{"host": "db", "port": 5432, "user": "u", "name": "pay"}}}}}}}},
            "cluster": {{
                "now": "2026-10-27T18:30:00Z",
                "nodes": ["worker-1", "worker-2"],
                "pods": [{{
                    "apiVersion": "v1", "kind": "Pod",
                    "metadata": {{"name": "p", "namespace": "default"}},
                    "spec": {{"containers": [{{"name": "app", "image": "app:1", "env": [{env}]}}]}}
                }}]
            }}
        }}"#
        )
    }

    const TYPO: &str = r#"{"name": "DATABSE_URL", "value": "postgres://u@db:5432/pay"}"#;
    const GOOD: &str = r#"{"name": "DATABASE_URL", "value": "postgres://u@db:5432/pay"}"#;

    fn row(e: &mut Engine) -> Vec<String> {
        let out = e.eval("kubectl get pod p");
        out.lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    #[test]
    fn crash_loop_restarts_every_ten_seconds() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        assert_eq!(row(&mut e)[..4], ["p", "0/1", "ContainerCreating", "0"]);

        // Started at +2 s, crashes 1 s later, waits a flat 10 s, starts again: +13, +24, +35, ...
        let timeline = [
            (2, "Running", "0"),
            (3, "CrashLoopBackOff", "0"),
            (12, "CrashLoopBackOff", "0"),
            (13, "Running", "1"),
            (14, "CrashLoopBackOff", "1"),
            (24, "Running", "2"),
            (25, "CrashLoopBackOff", "2"),
            (35, "Running", "3"),
        ];
        let mut elapsed = 0;
        for (at, status, restarts) in timeline {
            e.advance(at - elapsed);
            elapsed = at;
            let r = row(&mut e);
            assert_eq!((r[2].as_str(), r[3].as_str()), (status, restarts), "t=+{at}s: {r:?}");
        }
    }

    #[test]
    fn the_delay_never_grows() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        // Restart n happens at 13 + 11 (n - 1): the wait is 10 s however long the loop has run.
        e.advance(13 + 11 * 99);
        assert_eq!(row(&mut e)[3], "100");
        // It crashes a second after each start, then waits a flat 10 s for the next.
        e.advance(1);
        assert_eq!(row(&mut e)[2], "CrashLoopBackOff");
        let waiting = e.eval("kubectl get pod p -o jsonpath='{.status.containerStatuses[0].state.waiting.message}'");
        assert!(
            waiting.starts_with("back-off 10s restarting failed container=app pod=p_default("),
            "{waiting}"
        );
        e.advance(9);
        assert_eq!(row(&mut e)[3], "100", "still waiting");
        e.advance(1);
        assert_eq!(row(&mut e)[3], "101");
    }

    #[test]
    fn logs_are_written_per_instance_with_due_timestamps() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        e.advance(14);
        let store = e.store();
        let first = store.read_logs("default", "p", "app", 0).unwrap();
        let second = store.read_logs("default", "p", "app", 1).unwrap();
        assert_eq!(
            first.iter().map(|l| l.line.as_str()).collect::<Vec<_>>(),
            [
                "INFO  app 1 starting (pid 1)",
                "INFO  loading configuration from environment",
                "ERROR required environment variable DATABASE_URL is not set",
                "FATAL cannot start without a database connection, exiting",
            ]
        );
        assert_eq!(first[3].ts - first[0].ts, 1);
        assert_eq!(second[0].ts - first[0].ts, 11); // restarted 10 s after the first crash (+1 s run)
    }

    #[test]
    fn correct_env_runs_healthy_and_logs_what_the_app_prints() {
        let mut e = crate::scenario::inline::engine(&scenario(GOOD));
        e.advance(600);
        assert_eq!(row(&mut e)[1..4], ["1/1", "Running", "0"]);
        let lines = e.store().read_logs("default", "p", "app", 0).unwrap();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[3].line, "INFO  connected to postgres://u@db:5432/pay");
    }

    #[test]
    fn status_reflects_last_state_and_backoff_message() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        e.advance(4);
        let json: serde_json::Value = serde_json::from_str(&e.eval("kubectl get pod p -o json")).unwrap();
        let cs = &json["status"]["containerStatuses"][0];
        assert_eq!(cs["state"]["waiting"]["reason"], "CrashLoopBackOff");
        assert!(cs["state"]["waiting"]["message"]
            .as_str()
            .unwrap()
            .starts_with("back-off 10s restarting failed container=app pod=p_default("));
        assert_eq!(cs["lastState"]["terminated"]["exitCode"], 1);
        assert_eq!(cs["lastState"]["terminated"]["reason"], "Error");
        assert_eq!(cs["ready"], false);
        assert_eq!(json["status"]["phase"], "Running");
        let ready = json["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "Ready")
            .unwrap();
        assert_eq!(ready["status"], "False");
        assert_eq!(ready["message"], "containers with unready status: [app]");
    }

    #[test]
    fn scheduler_spreads_pods_and_records_events() {
        let json = r#"{"id":"t","cluster":{"now":"2026-10-27T18:30:00Z","nodes":["a","b"],"pods":[
            {"apiVersion":"v1","kind":"Pod","metadata":{"name":"p1"},"spec":{"containers":[{"name":"c","image":"x"}]}},
            {"apiVersion":"v1","kind":"Pod","metadata":{"name":"p2"},"spec":{"containers":[{"name":"c","image":"x"}]}},
            {"apiVersion":"v1","kind":"Pod","metadata":{"name":"p3"},"spec":{"containers":[{"name":"c","image":"x"}]}}
        ]}}"#;
        let e = crate::scenario::inline::engine(json);
        let nodes: Vec<String> = e
            .store()
            .list("Pod", "default")
            .unwrap()
            .iter()
            .map(|p| p["spec"]["nodeName"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(nodes, ["a", "b", "a"]);
        let reasons: Vec<String> = e
            .store()
            .list("Event", "default")
            .unwrap()
            .iter()
            .map(|ev| ev["reason"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(reasons.iter().filter(|r| *r == "Scheduled").count(), 3);
        assert_eq!(reasons.iter().filter(|r| *r == "Pulling").count(), 3);
    }

    #[test]
    fn no_nodes_means_unschedulable() {
        let json = r#"{"id":"t","cluster":{"now":"2026-10-27T18:30:00Z","pods":[
            {"apiVersion":"v1","kind":"Pod","metadata":{"name":"p"},"spec":{"containers":[{"name":"c","image":"x"}]}}]}}"#;
        let mut e = crate::scenario::inline::engine(json);
        e.advance(60);
        let out = e.eval("kubectl get pod p");
        assert!(out.lines().nth(1).unwrap().contains("Pending"), "{out}");
        let events = e.store().list("Event", "default").unwrap();
        assert_eq!(events[0]["reason"], "FailedScheduling");
    }

    #[test]
    fn events_follow_the_container_lifecycle() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        e.advance(14);
        let events = e.store().list("Event", "default").unwrap();
        let count = |reason: &str| {
            events
                .iter()
                .find(|ev| ev["reason"] == reason)
                .map_or(0, |ev| ev["count"].as_i64().unwrap())
        };
        assert_eq!(count("Scheduled"), 1);
        assert_eq!(count("Pulling"), 1);
        assert_eq!(count("Created"), 1 + 1); // first start + one restart (identical message => aggregated)
        assert_eq!(count("BackOff"), 1, "one BackOff per restart, matching RESTARTS");
    }

    #[test]
    fn static_authored_pods_are_left_alone() {
        let mut e = crate::scenario::inline::repo("sample-crashloop");
        let before = e.eval("kubectl get pods");
        e.advance(3600);
        assert_eq!(
            e.eval("kubectl get pods")
                .lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .take(4)
                .collect::<Vec<_>>(),
            before
                .lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .take(4)
                .collect::<Vec<_>>()
        );
        assert!(e.store().list("Event", "default").unwrap().is_empty());
    }

    #[test]
    fn advance_in_one_step_equals_many_steps() {
        let mut big = crate::scenario::inline::engine(&scenario(TYPO));
        big.advance(700);
        let mut small = crate::scenario::inline::engine(&scenario(TYPO));
        for _ in 0..700 {
            small.advance(1);
        }
        assert_eq!(
            big.eval("kubectl get pod p -o yaml"),
            small.eval("kubectl get pod p -o yaml")
        );
        assert_eq!(
            big.store().list("Event", "default").unwrap(),
            small.store().list("Event", "default").unwrap()
        );
    }

    #[test]
    fn deleted_pod_disappears_after_grace_and_takes_its_state_along() {
        let mut e = crate::scenario::inline::engine(&scenario(TYPO));
        e.advance(5);
        let mut pod: serde_json::Value = e.store().get("Pod", "default", "p").unwrap().unwrap();
        pod["metadata"]["deletionTimestamp"] = serde_json::json!(crate::clock::format(e.now_secs() + 2));
        e.store().update("Pod", "default", "p", &pod).unwrap();
        e.advance(0);
        assert!(e.store().get("Pod", "default", "p").unwrap().is_some());
        e.advance(2);
        assert!(e.store().get("Pod", "default", "p").unwrap().is_none());
        assert!(e.store().read_logs("default", "p", "app", 0).unwrap().is_empty());
        assert_eq!(e.store().next_wake().unwrap(), None);
    }

    #[test]
    fn go_durations() {
        assert_eq!(
            [10, 20, 40, 80, 160, 300].map(go_duration),
            ["10s", "20s", "40s", "1m20s", "2m40s", "5m0s"]
        );
        assert_eq!(go_duration(3600), "1h0m0s");
    }
}
