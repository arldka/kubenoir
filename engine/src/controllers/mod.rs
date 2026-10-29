//! The simulated control plane: controllers, scheduler and kubelet.
//!
//! `reconcile` runs every control loop at one instant of simulated time until the cluster stops
//! changing. Each loop must be idempotent and must stamp changes with the time they were *due*,
//! not the time they were observed, so that `advance(60)` equals sixty `advance(1)` calls.

pub mod config;
mod container_env;
mod cronjob;
mod deployment;
mod images;
mod job;
mod kubelet;
mod namespace;
mod pods;
mod probes;
mod replicaset;
mod scheduler;
mod service;
pub(crate) mod volumes;

pub use kubelet::{answer, is_running, listens, select_log_instance, LogSelection};
pub use pods::{delete_pod, is_owned_by};

use k8s_openapi::api::core::v1::Pod;

use crate::store::{ClusterStore, StoreError};
use config::ClusterConfig;

/// Upper bound on passes per instant; a loop that never settles is a bug, not a hang.
const MAX_PASSES: usize = 64;

/// Everything a control loop may read or write at one instant.
pub struct Env<'a> {
    pub store: &'a ClusterStore,
    pub config: &'a ClusterConfig,
    /// Simulated time, epoch seconds.
    pub now: i64,
}

/// Run all control loops at `env.now` until quiescent.
pub fn reconcile(env: &Env<'_>) -> Result<(), StoreError> {
    for _ in 0..MAX_PASSES {
        if !pass(env)? {
            return Ok(());
        }
    }
    debug_assert!(false, "control loops did not settle within {MAX_PASSES} passes");
    Ok(())
}

/// One sweep of every loop, in dependency order. Returns whether anything changed.
fn pass(env: &Env<'_>) -> Result<bool, StoreError> {
    // Namespaces being deleted go first, so their workloads are gone before they can react.
    let namespaces = namespace::pass(env)?;
    let deployments = deployment::pass(env)?;
    let replica_sets = replicaset::pass(env)?;
    let scheduled = scheduler::pass(env)?;
    let ran = kubelet::pass(env)?;
    let services = service::pass(env)?;
    let cron = cronjob::pass(env)?;
    let jobs = job::pass(env)?;
    Ok(namespaces || deployments || replica_sets || scheduled || ran || services || cron || jobs)
}

/// `(namespace, name, uid)` of a pod, with the defaults the API server would apply.
fn pod_identity(pod: &Pod) -> (String, String, String) {
    (
        pod.metadata
            .namespace
            .clone()
            .unwrap_or_else(|| crate::scenario::DEFAULT_NAMESPACE.to_string()),
        pod.metadata.name.clone().unwrap_or_default(),
        pod.metadata.uid.clone().unwrap_or_default(),
    )
}

fn pods(store: &ClusterStore) -> Result<Vec<Pod>, StoreError> {
    Ok(store
        .list_all("Pod")?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?)
}
