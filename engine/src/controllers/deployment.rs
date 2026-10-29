//! Deployment controller: one ReplicaSet per pod-template revision, rolled out and retired
//! following Kubernetes' rolling-update rules.
//!
//! A change to `spec.template` hashes to a new ReplicaSet. The controller scales it up within
//! `maxSurge`, and scales old ReplicaSets down only as far as `maxUnavailable` allows, removing
//! unhealthy old pods first. So a bad fix that never becomes Ready leaves the old pods in place.

use k8s_openapi::api::apps::v1::{Deployment, DeploymentCondition, DeploymentSpec, DeploymentStatus, ReplicaSet};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use serde_json::json;

use super::pods::{is_owned_by, owner_ref};
use super::Env;
use crate::clock;
use crate::events::{self, EventType, Involved, Source};
use crate::names;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::StoreError;

const HASH_LABEL: &str = "pod-template-hash";
const REVISION: &str = "deployment.kubernetes.io/revision";
const DESIRED: &str = "deployment.kubernetes.io/desired-replicas";
const MAX: &str = "deployment.kubernetes.io/max-replicas";
const DEFAULT_HISTORY_LIMIT: usize = 10;

pub fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    let mut changed = false;
    for manifest in env.store.list_all("Deployment")? {
        changed |= sync(env, serde_json::from_value(manifest)?)?;
    }
    Ok(changed)
}

fn sync(env: &Env<'_>, mut dep: Deployment) -> Result<bool, StoreError> {
    let before = serde_json::to_value(&dep)?;
    let namespace = dep
        .metadata
        .namespace
        .get_or_insert_with(|| DEFAULT_NAMESPACE.into())
        .clone();
    let name = dep.metadata.name.clone().unwrap_or_default();
    let uid = dep.metadata.uid.clone().unwrap_or_default();
    dep.metadata
        .creation_timestamp
        .get_or_insert_with(|| clock::to_time(env.now));
    dep.metadata.generation.get_or_insert(1);

    let mut did = false;
    if let Some(spec) = dep.spec.clone() {
        let desired = spec.replicas.unwrap_or(1).max(0);
        let hash = names::template_hash(&serde_json::to_value(&spec.template)?);
        let owned = owned_sets(env, &namespace, &uid)?;

        match owned
            .iter()
            .position(|rs| rs.metadata.labels.as_ref().and_then(|l| l.get(HASH_LABEL)) == Some(&hash))
        {
            None => {
                create_replica_set(env, &dep, &spec, &hash, &owned)?;
                did = true;
            }
            Some(new) => {
                did = rollout(env, &dep, &spec, &owned, new, desired)?;
                if !did {
                    did = reconcile_revision(env, &mut dep, &owned, new)?;
                }
                if !did {
                    update_status(env, &mut dep, &spec, &owned, new, desired);
                    did = prune_history(env, &spec, &owned, new)?;
                }
            }
        }
    }

    let after = serde_json::to_value(&dep)?;
    if after != before {
        env.store.update("Deployment", &namespace, &name, &after)?;
        did = true;
    }
    Ok(did)
}

fn owned_sets(env: &Env<'_>, namespace: &str, owner_uid: &str) -> Result<Vec<ReplicaSet>, StoreError> {
    let mut owned = Vec::new();
    for manifest in env.store.list("ReplicaSet", namespace)? {
        let rs: ReplicaSet = serde_json::from_value(manifest)?;
        if is_owned_by(&rs.metadata, owner_uid) {
            owned.push(rs);
        }
    }
    Ok(owned)
}

fn revision_of(rs: &ReplicaSet) -> i64 {
    rs.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(REVISION))
        .and_then(|r| r.parse().ok())
        .unwrap_or(0)
}

fn spec_replicas(rs: &ReplicaSet) -> i32 {
    rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1)
}

fn available(rs: &ReplicaSet) -> i32 {
    rs.status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0)
}

fn rs_name(rs: &ReplicaSet) -> String {
    rs.metadata.name.clone().unwrap_or_default()
}

fn create_replica_set(
    env: &Env<'_>,
    dep: &Deployment,
    spec: &DeploymentSpec,
    hash: &str,
    owned: &[ReplicaSet],
) -> Result<(), StoreError> {
    let namespace = dep.metadata.namespace.clone().unwrap_or_default();
    let dep_name = dep.metadata.name.clone().unwrap_or_default();
    let dep_uid = dep.metadata.uid.clone().unwrap_or_default();
    let name = format!("{dep_name}-{hash}");

    let mut template = serde_json::to_value(&spec.template)?;
    template["metadata"]["labels"][HASH_LABEL] = json!(hash);
    let mut selector = spec.selector.match_labels.clone().unwrap_or_default();
    selector.insert(HASH_LABEL.into(), hash.into());
    let labels = template["metadata"]["labels"].clone();

    let revision = owned.iter().map(revision_of).max().unwrap_or(0) + 1;
    let mut annotations = dep.metadata.annotations.clone().unwrap_or_default();
    annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
    annotations.insert(REVISION.into(), revision.to_string());
    let desired = spec.replicas.unwrap_or(1);
    annotations.insert(DESIRED.into(), desired.to_string());
    annotations.insert(MAX.into(), (desired + rolling_limits(spec, desired).0).to_string());

    let rs = json!({
        "apiVersion": "apps/v1",
        "kind": "ReplicaSet",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": labels,
            "annotations": annotations,
            "creationTimestamp": clock::format(env.now),
            "generation": 1,
            "ownerReferences": [owner_ref("apps/v1", "Deployment", &dep_name, &dep_uid)],
        },
        // Scaled up by the rollout step so surge limits apply.
        "spec": { "replicas": 0, "selector": { "matchLabels": selector }, "template": template },
    });
    env.store.insert("ReplicaSet", "apps/v1", &namespace, &name, &rs)
}

/// Max surge / max unavailable as absolute pod counts (percentages: surge rounds up,
/// unavailable rounds down; they may not both be zero).
fn rolling_limits(spec: &DeploymentSpec, desired: i32) -> (i32, i32) {
    let rolling = spec.strategy.as_ref().and_then(|s| s.rolling_update.as_ref());
    let resolve = |value: Option<&IntOrString>, round_up: bool| match value {
        Some(IntOrString::Int(n)) => *n,
        Some(IntOrString::String(s)) => percent(s, desired, round_up),
        None => percent("25%", desired, round_up),
    };
    let surge = resolve(rolling.and_then(|r| r.max_surge.as_ref()), true);
    let mut unavailable = resolve(rolling.and_then(|r| r.max_unavailable.as_ref()), false);
    if surge == 0 && unavailable == 0 {
        unavailable = 1;
    }
    (surge, unavailable)
}

fn percent(text: &str, of: i32, round_up: bool) -> i32 {
    let pct: i32 = text.trim_end_matches('%').parse().unwrap_or(25);
    let scaled = of * pct;
    if round_up {
        (scaled + 99) / 100
    } else {
        scaled / 100
    }
}

/// Scale the new ReplicaSet up (within surge) and old ones down (within availability).
/// Does one kind of change per call; the reconcile loop comes back for the rest.
fn rollout(
    env: &Env<'_>,
    dep: &Deployment,
    spec: &DeploymentSpec,
    owned: &[ReplicaSet],
    new: usize,
    desired: i32,
) -> Result<bool, StoreError> {
    let recreate = spec.strategy.as_ref().and_then(|s| s.type_.as_deref()) == Some("Recreate");
    let (max_surge, max_unavailable) = rolling_limits(spec, desired);
    let mut replicas: Vec<i32> = owned.iter().map(spec_replicas).collect();
    let total: i32 = replicas.iter().sum();

    let target_new = if recreate {
        // Tear everything old down and wait for it to be gone before starting the new revision.
        let mut scaled = false;
        for (i, rs) in owned.iter().enumerate().filter(|(i, _)| *i != new) {
            if replicas[i] > 0 {
                scale(env, dep, rs, 0)?;
                scaled = true;
            }
        }
        let old_running: i32 = owned
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != new)
            .map(|(_, rs)| rs.status.as_ref().map_or(0, |s| s.replicas))
            .sum();
        if scaled || old_running > 0 {
            return Ok(scaled);
        }
        desired
    } else if replicas[new] > desired {
        desired
    } else {
        let room = (desired + max_surge - total).max(0);
        (replicas[new] + room).min(desired)
    };
    if target_new != replicas[new] {
        scale(env, dep, &owned[new], target_new)?;
        return Ok(true);
    }
    if recreate {
        return Ok(false);
    }

    // Scale old ReplicaSets down, mirroring the Kubernetes deployment controller.
    let min_available = (desired - max_unavailable).max(0);
    let new_unavailable = replicas[new] - available(&owned[new]);
    let mut budget = total - min_available - new_unavailable;
    if budget <= 0 {
        return Ok(false);
    }
    let mut old: Vec<usize> = (0..owned.len()).filter(|i| *i != new && replicas[*i] > 0).collect();
    old.sort_by_key(|i| {
        (
            owned[*i].metadata.creation_timestamp.as_ref().map_or(0, clock::secs),
            rs_name(&owned[*i]),
        )
    });

    let mut changed = false;
    for &i in &old {
        let unhealthy = (replicas[i] - available(&owned[i])).max(0).min(budget);
        if unhealthy > 0 {
            replicas[i] -= unhealthy;
            budget -= unhealthy;
            scale(env, dep, &owned[i], replicas[i])?;
            changed = true;
        }
    }
    let available_total: i32 = owned.iter().map(available).sum();
    let mut healthy_budget = available_total - min_available;
    for &i in &old {
        if healthy_budget <= 0 {
            break;
        }
        let cut = replicas[i].min(healthy_budget);
        if cut > 0 {
            replicas[i] -= cut;
            healthy_budget -= cut;
            scale(env, dep, &owned[i], replicas[i])?;
            changed = true;
        }
    }
    Ok(changed)
}

fn scale(env: &Env<'_>, dep: &Deployment, rs: &ReplicaSet, to: i32) -> Result<(), StoreError> {
    let from = spec_replicas(rs);
    let namespace = rs.metadata.namespace.clone().unwrap_or_default();
    let name = rs_name(rs);
    let mut updated = rs.clone();
    if let Some(spec) = updated.spec.as_mut() {
        spec.replicas = Some(to);
    }
    let annotations = updated.metadata.annotations.get_or_insert_with(Default::default);
    let desired = dep.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
    annotations.insert(DESIRED.into(), desired.to_string());
    if let Some(spec) = dep.spec.as_ref() {
        annotations.insert(MAX.into(), (desired + rolling_limits(spec, desired).0).to_string());
    }
    env.store
        .update("ReplicaSet", &namespace, &name, &serde_json::to_value(&updated)?)?;

    let dep_name = dep.metadata.name.clone().unwrap_or_default();
    let uid = dep.metadata.uid.clone().unwrap_or_default();
    let involved = Involved {
        kind: "Deployment",
        api_version: "apps/v1",
        namespace: &namespace,
        name: &dep_name,
        uid: &uid,
        field_path: None,
    };
    let direction = if to > from { "up" } else { "down" };
    events::record(
        env.store,
        &involved,
        EventType::Normal,
        "ScalingReplicaSet",
        &format!("Scaled {direction} replica set {name} from {from} to {to}"),
        &Source {
            component: "deployment-controller",
            host: None,
        },
        env.now,
    )
}

/// Keep revision annotations right: the active ReplicaSet always carries the highest revision
/// (matters when a rollout goes back to an earlier template), and the Deployment mirrors it.
fn reconcile_revision(
    env: &Env<'_>,
    dep: &mut Deployment,
    owned: &[ReplicaSet],
    new: usize,
) -> Result<bool, StoreError> {
    let max_other = owned
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != new)
        .map(|(_, rs)| revision_of(rs))
        .max()
        .unwrap_or(0);
    let mut revision = revision_of(&owned[new]);
    let mut changed = false;
    if revision <= max_other {
        revision = max_other + 1;
        let mut updated = owned[new].clone();
        updated
            .metadata
            .annotations
            .get_or_insert_with(Default::default)
            .insert(REVISION.into(), revision.to_string());
        let namespace = updated.metadata.namespace.clone().unwrap_or_default();
        env.store.update(
            "ReplicaSet",
            &namespace,
            &rs_name(&updated),
            &serde_json::to_value(&updated)?,
        )?;
        changed = true;
    }
    dep.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert(REVISION.into(), revision.to_string());
    Ok(changed)
}

fn update_status(
    env: &Env<'_>,
    dep: &mut Deployment,
    spec: &DeploymentSpec,
    owned: &[ReplicaSet],
    new: usize,
    desired: i32,
) {
    let sum = |f: &dyn Fn(&ReplicaSet) -> i32| owned.iter().map(f).sum::<i32>();
    let replicas = sum(&|rs| rs.status.as_ref().map_or(0, |s| s.replicas));
    let ready = sum(&|rs| rs.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0));
    let available_total = sum(&available);
    let updated = owned[new].status.as_ref().map_or(0, |s| s.replicas);
    let (_, max_unavailable) = rolling_limits(spec, desired);

    let complete = spec_replicas(&owned[new]) == desired
        && updated == desired
        && available(&owned[new]) == desired
        && replicas == updated;
    let have_minimum = available_total >= (desired - max_unavailable).max(0);
    let new_name = rs_name(&owned[new]);

    let old = dep.status.take().unwrap_or_default();
    let keep_time = |ty: &str, status: &str| {
        old.conditions
            .iter()
            .flatten()
            .find(|c| c.type_ == ty && c.status == status)
            .and_then(|c| c.last_transition_time.clone())
            .unwrap_or_else(|| clock::to_time(env.now))
    };
    let condition = |ty: &str, ok: bool, reason: &str, message: String| {
        let status = if ok { "True" } else { "False" };
        let unchanged = old
            .conditions
            .iter()
            .flatten()
            .any(|c| c.type_ == ty && c.status == status && c.reason.as_deref() == Some(reason));
        DeploymentCondition {
            type_: ty.into(),
            status: status.into(),
            reason: Some(reason.into()),
            message: Some(message),
            last_transition_time: Some(keep_time(ty, status)),
            last_update_time: Some(if unchanged {
                old.conditions
                    .iter()
                    .flatten()
                    .find(|c| c.type_ == ty)
                    .and_then(|c| c.last_update_time.clone())
                    .unwrap_or_else(|| clock::to_time(env.now))
            } else {
                clock::to_time(env.now)
            }),
        }
    };
    let available_condition = if have_minimum {
        condition(
            "Available",
            true,
            "MinimumReplicasAvailable",
            "Deployment has minimum availability.".into(),
        )
    } else {
        condition(
            "Available",
            false,
            "MinimumReplicasUnavailable",
            "Deployment does not have minimum availability.".into(),
        )
    };
    let progressing = if complete {
        condition(
            "Progressing",
            true,
            "NewReplicaSetAvailable",
            format!("ReplicaSet \"{new_name}\" has successfully progressed."),
        )
    } else {
        condition(
            "Progressing",
            true,
            "ReplicaSetUpdated",
            format!("ReplicaSet \"{new_name}\" is progressing."),
        )
    };

    dep.status = Some(DeploymentStatus {
        observed_generation: dep.metadata.generation,
        replicas: Some(replicas),
        updated_replicas: Some(updated),
        ready_replicas: Some(ready),
        available_replicas: Some(available_total),
        unavailable_replicas: Some((replicas - available_total).max(0)).filter(|n| *n > 0),
        conditions: Some(vec![available_condition, progressing]),
        ..Default::default()
    });
}

/// Delete the oldest fully scaled-down ReplicaSets beyond `revisionHistoryLimit`.
fn prune_history(env: &Env<'_>, spec: &DeploymentSpec, owned: &[ReplicaSet], new: usize) -> Result<bool, StoreError> {
    let limit = spec
        .revision_history_limit
        .map_or(DEFAULT_HISTORY_LIMIT, |l| l.max(0) as usize);
    let mut idle: Vec<&ReplicaSet> = owned
        .iter()
        .enumerate()
        .filter(|(i, rs)| *i != new && spec_replicas(rs) == 0 && rs.status.as_ref().map_or(0, |s| s.replicas) == 0)
        .map(|(_, rs)| rs)
        .collect();
    if idle.len() <= limit {
        return Ok(false);
    }
    idle.sort_by_key(|rs| revision_of(rs));
    for rs in idle.into_iter().take(idle_excess(owned, new, limit)) {
        let namespace = rs.metadata.namespace.clone().unwrap_or_default();
        env.store.delete("ReplicaSet", &namespace, &rs_name(rs))?;
    }
    Ok(true)
}

fn idle_excess(owned: &[ReplicaSet], new: usize, limit: usize) -> usize {
    let idle = owned
        .iter()
        .enumerate()
        .filter(|(i, rs)| *i != new && spec_replicas(rs) == 0 && rs.status.as_ref().map_or(0, |s| s.replicas) == 0)
        .count();
    idle.saturating_sub(limit)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::sim::Engine;

    const NOW: &str = "2026-10-27T18:30:00Z";

    /// `payments-api` runs the `db-client` app, which needs DATABASE_URL (it crashes within a
    /// second otherwise), but its Deployment sets the typo'd DATABSE_URL. `web` is a healthy
    /// silent 2-replica workload.
    fn scenario(started_at: &str, extra_deployments: &str) -> String {
        format!(
            r#"{{
            "id": "w",
            "images": {{"app:1": {{"app": "db-client@0.1.0", "config": {{"name": "payments-api", "version": "1",
                "database": {{"host": "db", "port": 5432, "user": "u", "name": "pay"}}}}}}}},
            "cluster": {{
                "startedAt": "{started_at}", "now": "{NOW}", "nodes": ["worker-1", "worker-2"],
                "deployments": [
                    {{"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {{"name": "payments-api", "labels": {{"app": "payments-api"}}}},
                      "spec": {{"replicas": 1, "selector": {{"matchLabels": {{"app": "payments-api"}}}},
                        "template": {{"metadata": {{"labels": {{"app": "payments-api"}}}},
                          "spec": {{"containers": [{{"name": "payments-api", "image": "app:1", "env": [{{"name": "DATABSE_URL", "value": "postgres://u@db:5432/pay"}}]}}]}}}}}}}}
                    {extra_deployments}
                ]
            }}
        }}"#
        )
    }

    const WEB: &str = r#",{"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "web"},
        "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "web"}},
          "template": {"metadata": {"labels": {"app": "web"}}, "spec": {"containers": [{"name": "web", "image": "nginx:1.27"}]}}}}"#;

    fn engine(started_at: &str) -> Engine {
        crate::scenario::inline::engine(&scenario(started_at, WEB))
    }

    /// Edit a Deployment the way `kubectl` would: change it and bump `generation`.
    fn patch(e: &mut Engine, name: &str, edit: impl FnOnce(&mut Value)) {
        let mut d = e.store().get("Deployment", "default", name).unwrap().unwrap();
        edit(&mut d);
        let generation = d["metadata"]["generation"].as_i64().unwrap_or(1) + 1;
        d["metadata"]["generation"] = json!(generation);
        e.store().update("Deployment", "default", name, &d).unwrap();
        e.advance(0); // out-of-band edit: let the control plane notice it
    }

    fn set_env_name(d: &mut Value, name: &str) {
        d["spec"]["template"]["spec"]["containers"][0]["env"][0]["name"] = json!(name);
    }

    /// `(name, ready, status, restarts)` rows of `kubectl get pods`.
    fn pods(e: &mut Engine) -> Vec<(String, String, String, String)> {
        let out = e.eval("kubectl get pods");
        out.lines()
            .skip(1)
            .filter(|l| !l.starts_with("No resources"))
            .map(|l| {
                let c: Vec<&str> = l.split_whitespace().collect();
                (c[0].to_string(), c[1].to_string(), c[2].to_string(), c[3].to_string())
            })
            .collect()
    }

    fn payments(e: &mut Engine) -> Vec<(String, String, String, String)> {
        pods(e)
            .into_iter()
            .filter(|p| p.0.starts_with("payments-api-"))
            .collect()
    }

    #[test]
    fn deployments_get_replicasets_and_pods_spread_across_nodes() {
        let mut e = engine("2026-10-27T17:30:00Z");
        let deploy = e.eval("kubectl get deployments");
        assert_eq!(
            deploy.lines().nth(2).unwrap().split_whitespace().collect::<Vec<_>>(),
            ["web", "2/2", "2", "2", "60m"]
        );

        let rs = e.eval("kubectl get rs");
        let web_rs = rs
            .lines()
            .find(|l| l.starts_with("web-"))
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(web_rs[1..], ["2", "2", "2", "60m"]);

        let web: Vec<_> = pods(&mut e).into_iter().filter(|p| p.0.starts_with("web-")).collect();
        assert_eq!(web.len(), 2);
        assert!(
            web.iter()
                .all(|p| p.0.starts_with(web_rs[0]) && p.1 == "1/1" && p.2 == "Running"),
            "{web:?}"
        );

        let wide = e.eval("kubectl get pods -o wide");
        let nodes: std::collections::BTreeSet<&str> = wide
            .lines()
            .filter(|l| l.starts_with("web-"))
            .map(|l| l.split_whitespace().nth(6).unwrap())
            .collect();
        assert_eq!(nodes.len(), 2, "{wide}");
    }

    #[test]
    fn warm_up_replays_the_crash_loop_history() {
        let mut e = engine("2026-10-27T17:43:00Z"); // 47 minutes of history
        let p = payments(&mut e);
        assert_eq!(p.len(), 1);
        // A restart every 11 s (1 s crash, 10 s wait) from +13 s: 256 of them by +2820 s.
        assert_eq!(
            (p[0].1.as_str(), p[0].2.as_str(), p[0].3.as_str()),
            ("0/1", "CrashLoopBackOff", "256")
        );
        let table = e.eval("kubectl get pods");
        assert!(
            table
                .lines()
                .any(|l| l.starts_with("payments-api-") && l.ends_with("47m")),
            "{table}"
        );
        assert_eq!(e.now(), NOW);
    }

    #[test]
    fn deployment_status_describes_the_unavailable_replica() {
        let mut e = engine("2026-10-27T17:43:00Z");
        let out = e.eval("kubectl describe deployment payments-api");
        for expected in [
            "Name:                   payments-api",
            "Selector:               app=payments-api",
            "Replicas:               1 desired | 1 updated | 1 total | 0 available | 1 unavailable",
            "StrategyType:           RollingUpdate",
            "RollingUpdateStrategy:  25% max unavailable, 25% max surge",
            "OldReplicaSets:  <none>",
            "      DATABSE_URL:  postgres://u@db:5432/pay",
            "  Available      False   MinimumReplicasUnavailable",
            "  Progressing    True    ReplicaSetUpdated",
        ] {
            assert!(out.lines().any(|l| l == expected), "missing {expected:?} in:\n{out}");
        }
        assert!(
            out.lines()
                .any(|l| l.starts_with("NewReplicaSet:   payments-api-") && l.ends_with("(1/1 replicas created)")),
            "{out}"
        );
        assert!(
            out.contains("Annotations:            deployment.kubernetes.io/revision: 1"),
            "{out}"
        );
        assert!(
            out.lines().any(|l| l.contains("ScalingReplicaSet")
                && l.contains("Scaled up replica set payments-api-")
                && l.ends_with("from 0 to 1")),
            "{out}"
        );
    }

    #[test]
    fn fixing_the_env_rolls_out_a_healthy_pod_and_retires_the_crashing_one() {
        let mut e = engine("2026-10-27T17:43:00Z");
        let old = payments(&mut e)[0].0.clone();
        patch(&mut e, "payments-api", |d| set_env_name(d, "DATABASE_URL"));

        // The new pod appears immediately; the old one stays until the new one is available.
        let during = payments(&mut e);
        assert_eq!(during.len(), 2, "{during:?}");
        let new = during.iter().find(|p| p.0 != old).unwrap();
        assert_eq!((new.1.as_str(), new.2.as_str()), ("0/1", "ContainerCreating"));
        assert!(during.iter().any(|p| p.0 == old && p.2 == "CrashLoopBackOff"));

        e.advance(2); // image pulled, container running but not yet Ready: nothing is retired
        let running = payments(&mut e);
        assert!(
            running.iter().any(|p| p.0 != old && p.1 == "0/1" && p.2 == "Running"),
            "{running:?}"
        );
        assert!(
            running.iter().any(|p| p.0 == old && p.2 == "CrashLoopBackOff"),
            "{running:?}"
        );

        e.advance(1); // Ready -> the rollout may retire the old pod
        let after_ready = payments(&mut e);
        assert!(
            after_ready.iter().any(|p| p.0 == old && p.2 == "Terminating"),
            "{after_ready:?}"
        );
        assert!(
            after_ready
                .iter()
                .any(|p| p.0 != old && p.1 == "1/1" && p.2 == "Running"),
            "{after_ready:?}"
        );

        e.advance(2); // grace period over
        let settled = payments(&mut e);
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(
            (settled[0].1.as_str(), settled[0].2.as_str(), settled[0].3.as_str()),
            ("1/1", "Running", "0")
        );
        assert_ne!(settled[0].0, old);

        let rs = e.eval("kubectl get rs");
        let counts: Vec<Vec<&str>> = rs
            .lines()
            .filter(|l| l.starts_with("payments-api-"))
            .map(|l| l.split_whitespace().skip(1).take(3).collect())
            .collect();
        assert_eq!(counts.len(), 2, "{rs}");
        assert!(
            counts.contains(&vec!["0", "0", "0"]) && counts.contains(&vec!["1", "1", "1"]),
            "{rs}"
        );

        let deploy = e.eval("kubectl describe deployment payments-api");
        assert!(
            deploy.contains("1 desired | 1 updated | 1 total | 1 available | 0 unavailable"),
            "{deploy}"
        );
        assert!(deploy.contains("deployment.kubernetes.io/revision: 2"), "{deploy}");
        assert!(deploy.contains("OldReplicaSets:  <none>"), "{deploy}");
        assert!(
            deploy.contains("  Available      True    MinimumReplicasAvailable"),
            "{deploy}"
        );
        assert!(
            deploy.contains("  Progressing    True    NewReplicaSetAvailable"),
            "{deploy}"
        );
        assert!(
            deploy.contains("Scaled down replica set payments-api-") && deploy.contains("from 1 to 0"),
            "{deploy}"
        );

        // The fixed container connects with the env value and serves.
        let logs = e.eval("kubectl logs deploy/payments-api");
        assert!(
            logs.ends_with("INFO  connected to postgres://u@db:5432/pay\nINFO  listening on :"),
            "{logs}"
        );
    }

    #[test]
    fn a_wrong_fix_leaves_the_old_pod_in_place() {
        let mut e = engine("2026-10-27T17:43:00Z");
        patch(&mut e, "payments-api", |d| set_env_name(d, "DATABASE_UR")); // still wrong
        e.advance(600);
        let p = payments(&mut e);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p.iter().all(|r| r.2 == "CrashLoopBackOff"), "{p:?}");
        let deploy = e.eval("kubectl get deployment payments-api");
        assert_eq!(
            deploy
                .lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .take(4)
                .collect::<Vec<_>>(),
            ["payments-api", "0/1", "1", "0"]
        );
    }

    #[test]
    fn deleted_pods_are_recreated_by_the_replicaset() {
        let mut e = engine("2026-10-27T17:30:00Z");
        let victim = pods(&mut e).into_iter().find(|p| p.0.starts_with("web-")).unwrap().0;
        e.store().delete("Pod", "default", &victim).unwrap();
        e.advance(0);
        let after = pods(&mut e);
        assert!(after.iter().all(|p| p.0 != victim));
        assert_eq!(after.iter().filter(|p| p.0.starts_with("web-")).count(), 2);
        let events = e.store().list("Event", "default").unwrap();
        assert!(events.iter().any(|ev| ev["reason"] == "SuccessfulCreate"
            && ev["count"].as_i64() == Some(1)
            && ev["involvedObject"]["kind"] == "ReplicaSet"));
    }

    #[test]
    fn scaling_up_and_down_changes_the_pod_count_without_a_new_replicaset() {
        let mut e = engine("2026-10-27T17:30:00Z");
        patch(&mut e, "web", |d| d["spec"]["replicas"] = json!(4));
        e.advance(3);
        assert_eq!(
            pods(&mut e)
                .iter()
                .filter(|p| p.0.starts_with("web-") && p.2 == "Running")
                .count(),
            4
        );

        patch(&mut e, "web", |d| d["spec"]["replicas"] = json!(1));
        e.advance(3);
        assert_eq!(pods(&mut e).iter().filter(|p| p.0.starts_with("web-")).count(), 1);
        let rs = e.eval("kubectl get rs");
        assert_eq!(rs.lines().filter(|l| l.starts_with("web-")).count(), 1, "{rs}");
        let events = e.store().list("Event", "default").unwrap();
        assert!(events
            .iter()
            .any(|ev| ev["reason"] == "SuccessfulDelete"
                && ev["message"].as_str().unwrap().starts_with("Deleted pod: web-")));
    }

    #[test]
    fn rolling_update_never_exceeds_surge_or_drops_below_availability() {
        let mut e = engine("2026-10-27T17:30:00Z");
        patch(&mut e, "web", |d| {
            d["spec"]["replicas"] = json!(4);
        });
        e.advance(10);
        patch(&mut e, "web", |d| {
            d["spec"]["template"]["spec"]["containers"][0]["image"] = json!("nginx:1.28")
        });

        // maxSurge = ceil(25% of 4) = 1, maxUnavailable = floor(25% of 4) = 1.
        let mut saw_new = false;
        for _ in 0..40 {
            let web: Vec<_> = pods(&mut e).into_iter().filter(|p| p.0.starts_with("web-")).collect();
            let live = web.iter().filter(|p| p.2 != "Terminating").count();
            let ready = web.iter().filter(|p| p.1 == "1/1" && p.2 == "Running").count();
            assert!(live <= 5, "surge exceeded: {web:?}");
            assert!(ready >= 3, "availability dropped: {web:?}");
            saw_new |= web.iter().any(|p| p.2 == "ContainerCreating");
            e.advance(1);
        }
        assert!(saw_new);
        let wide = e.eval("kubectl get deploy web -o wide");
        assert!(wide.contains("nginx:1.28"), "{wide}");
        let web: Vec<_> = pods(&mut e).into_iter().filter(|p| p.0.starts_with("web-")).collect();
        assert_eq!(web.len(), 4);
        assert!(web.iter().all(|p| p.2 == "Running"), "{web:?}");
    }

    #[test]
    fn rolling_back_to_an_earlier_template_reuses_its_replicaset_with_a_new_revision() {
        let mut e = engine("2026-10-27T17:43:00Z");
        patch(&mut e, "payments-api", |d| set_env_name(d, "DATABASE_URL"));
        e.advance(10);
        patch(&mut e, "payments-api", |d| set_env_name(d, "DATABSE_URL")); // back to the original
        e.advance(1);
        let rs = e.eval("kubectl get rs");
        assert_eq!(rs.lines().filter(|l| l.starts_with("payments-api-")).count(), 2, "{rs}");
        let deploy = e.eval("kubectl describe deployment payments-api");
        assert!(deploy.contains("deployment.kubernetes.io/revision: 3"), "{deploy}");
    }

    #[test]
    fn identical_scenarios_produce_identical_clusters() {
        let mut a = engine("2026-10-27T17:43:00Z");
        let mut b = engine("2026-10-27T17:43:00Z");
        for cmd in [
            "kubectl get pods -o yaml",
            "kubectl get rs -o yaml",
            "kubectl get events -o yaml",
            "kubectl get deploy -o yaml",
        ] {
            assert_eq!(a.eval(cmd), b.eval(cmd), "{cmd}");
        }
    }

    #[test]
    fn invalid_timelines_are_rejected() {
        let after =
            crate::scenario::inline::load(&scenario("2026-10-28T00:00:00Z", WEB), crate::apps::AppSet::bundled())
                .err()
                .unwrap();
        assert!(after.to_string().contains("startedAt is after"), "{after}");
        let too_old =
            crate::scenario::inline::load(&scenario("2026-01-01T00:00:00Z", WEB), crate::apps::AppSet::bundled())
                .err()
                .unwrap();
        assert!(too_old.to_string().contains("at most 14"), "{too_old}");
    }
}
