//! Scenario loading: parses an open-source scenario JSON file and seeds a `ClusterStore`.
//!
//! Unknown fields are ignored so scenario files can carry data the engine does not use yet
//! (narrative, win rules, ...).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde::Deserialize;

use crate::api;
use crate::apps::{self, AppSet};
use crate::controllers::config::{ClusterConfig, ImageSpec};
use crate::judge::{Finding, Rules, Success};
use crate::names;
use crate::store::{ClusterStore, StoreError};

pub const DEFAULT_NAMESPACE: &str = "default";

#[derive(Debug, Deserialize)]
struct Scenario {
    /// Seeds deterministic uids and generated names.
    #[serde(default)]
    id: String,
    /// What each image runs; see `controllers::config`.
    #[serde(default)]
    images: BTreeMap<String, ImageSpec>,
    /// What counts as solving the case; see `judge`.
    #[serde(default)]
    success: Option<Success>,
    /// Hidden clues unlocked by investigating; see `judge`.
    #[serde(default)]
    findings: Vec<Finding>,
    cluster: ClusterSeed,
}

#[derive(Debug, Deserialize)]
struct ClusterSeed {
    /// The simulated "current time", so AGE columns are deterministic.
    now: Time,
    /// When the simulation begins. Objects are created then and the cluster is run forward to
    /// `now`, so restart counts, logs, events and ages come out of the simulation itself.
    #[serde(default, rename = "startedAt")]
    started_at: Option<Time>,
    /// Node names pods can be scheduled onto.
    #[serde(default)]
    nodes: Vec<String>,
    #[serde(default)]
    pods: Vec<Pod>,
    /// Deployments; their ReplicaSets and pods are created by the simulated controllers.
    #[serde(default)]
    deployments: Vec<Deployment>,
}

#[derive(Debug)]
pub enum ScenarioError {
    Parse(serde_json::Error),
    MissingName { kind: &'static str, index: usize },
    Timeline(String),
    Rules(String),
    Image(String),
    Store(StoreError),
    Api(api::ApiError),
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScenarioError::Parse(e) => write!(f, "invalid scenario JSON: {e}"),
            ScenarioError::MissingName { kind, index } => {
                write!(f, "invalid scenario: {kind} #{index} has no metadata.name")
            }
            ScenarioError::Timeline(reason) => write!(f, "invalid scenario: {reason}"),
            ScenarioError::Rules(reason) => write!(f, "invalid scenario rules: {reason}"),
            ScenarioError::Image(reason) => write!(f, "invalid scenario image: {reason}"),
            ScenarioError::Store(e) => write!(f, "invalid scenario: {e}"),
            ScenarioError::Api(e) => write!(f, "invalid scenario: {}", e.message()),
        }
    }
}

impl std::error::Error for ScenarioError {}

impl From<api::ApiError> for ScenarioError {
    fn from(e: api::ApiError) -> Self {
        match e {
            api::ApiError::Store(e) => ScenarioError::Store(e),
            other => ScenarioError::Api(other),
        }
    }
}

impl From<StoreError> for ScenarioError {
    fn from(e: StoreError) -> Self {
        ScenarioError::Store(e)
    }
}

/// A loaded cluster: seeded store, control-loop configuration and the simulated current time.
pub struct Cluster {
    pub store: ClusterStore,
    pub config: ClusterConfig,
    /// Success criteria and findings, already validated.
    pub rules: Rules,
    /// When the simulation begins (`cluster.startedAt`, defaulting to `now`).
    pub started_at: Time,
    /// The time the player starts at; the cluster is run forward from `started_at` to here.
    pub now: Time,
}

/// Longest history a scenario may ask to have simulated before the player starts.
pub const MAX_WARMUP_SECS: i64 = 14 * 86_400;

/// The app references (`name@version`) a scenario's images run, sorted and deduplicated: what
/// the caller must fetch from an app registry and pass to `load`.
pub fn required_apps(json: &str) -> Result<Vec<String>, ScenarioError> {
    #[derive(Deserialize)]
    struct Images {
        #[serde(default)]
        images: BTreeMap<String, ImageSpec>,
    }
    let parsed: Images = serde_json::from_str(json).map_err(ScenarioError::Parse)?;
    let apps: BTreeSet<String> = parsed.images.into_values().filter_map(|spec| spec.app).collect();
    Ok(apps.into_iter().collect())
}

pub fn load(json: &str, apps: AppSet) -> Result<Cluster, ScenarioError> {
    let scenario: Scenario = serde_json::from_str(json).map_err(ScenarioError::Parse)?;
    for (image, spec) in &scenario.images {
        let Some(app) = spec.app.as_deref() else { continue };
        if !apps::is_reference(app) {
            return Err(ScenarioError::Image(format!(
                "{image} runs app {app:?}; app references must be name@version, e.g. db-client@0.1.0"
            )));
        }
        if !apps.contains(app) {
            return Err(ScenarioError::Image(format!(
                "{image} runs app {app}, which was not supplied (is it in the app registry?)"
            )));
        }
    }
    let store = ClusterStore::new()?;
    store.set_seed(names::seed_for(&scenario.id));
    let started_at = scenario
        .cluster
        .started_at
        .clone()
        .unwrap_or_else(|| scenario.cluster.now.clone());
    api::seed_system_namespaces(&store, crate::clock::secs(&started_at))?;

    for (index, mut pod) in scenario.cluster.pods.into_iter().enumerate() {
        let name = pod
            .metadata
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .ok_or(ScenarioError::MissingName { kind: "Pod", index })?;
        let namespace = pod
            .metadata
            .namespace
            .get_or_insert_with(|| DEFAULT_NAMESPACE.to_string())
            .clone();
        let manifest = serde_json::to_value(&pod).map_err(ScenarioError::Parse)?;
        store.insert("Pod", "v1", &namespace, &name, &manifest)?;
    }

    for (index, mut deployment) in scenario.cluster.deployments.into_iter().enumerate() {
        let name = deployment
            .metadata
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .ok_or(ScenarioError::MissingName {
                kind: "Deployment",
                index,
            })?;
        let namespace = deployment
            .metadata
            .namespace
            .get_or_insert_with(|| DEFAULT_NAMESPACE.to_string())
            .clone();
        let manifest = serde_json::to_value(&deployment).map_err(ScenarioError::Parse)?;
        store.insert("Deployment", "apps/v1", &namespace, &name, &manifest)?;
    }

    let now = scenario.cluster.now;
    let warmup = now.0.as_second() - started_at.0.as_second();
    if warmup < 0 {
        return Err(ScenarioError::Timeline("cluster.startedAt is after cluster.now".into()));
    }
    if warmup > MAX_WARMUP_SECS {
        return Err(ScenarioError::Timeline(format!(
            "cluster.startedAt is {} days before cluster.now; at most {} are supported",
            warmup / 86_400,
            MAX_WARMUP_SECS / 86_400
        )));
    }

    let rules = Rules {
        success: scenario.success,
        findings: scenario.findings,
    };
    rules.validate().map_err(ScenarioError::Rules)?;

    let config = ClusterConfig {
        nodes: scenario.cluster.nodes,
        images: scenario.images,
        apps,
    };
    Ok(Cluster {
        store,
        config,
        rules,
        started_at,
        now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-27T18:30:00Z";

    fn load_empty(json: &str) -> Result<Cluster, ScenarioError> {
        load(json, AppSet::default())
    }

    fn pod(extra_meta: &str) -> String {
        format!(r#"{{"apiVersion":"v1","kind":"Pod","metadata":{{{extra_meta}}},"spec":{{"containers":[]}}}}"#)
    }

    fn scenario(pods: &[String]) -> String {
        format!(
            r#"{{"id":"t","title":"T","cluster":{{"now":"{NOW}","pods":[{}]}}}}"#,
            pods.join(",")
        )
    }

    #[test]
    fn loads_pods_and_defaults_namespace() {
        let cluster = load_empty(&scenario(&[
            pod(r#""name":"a""#),
            pod(r#""name":"b","namespace":"prod""#),
        ]))
        .unwrap();
        assert!(cluster.store.get("Pod", "default", "a").unwrap().is_some());
        assert!(cluster.store.get("Pod", "prod", "b").unwrap().is_some());
        assert_eq!(
            cluster.store.get("Pod", "default", "a").unwrap().unwrap()["metadata"]["namespace"],
            "default"
        );
    }

    fn with_app(app: &str) -> String {
        format!(
            r#"{{"id":"t","images":{{"img:1":{{"app":"{app}"}},"img:2":{{"app":"{app}"}},"img:3":{{}}}},"cluster":{{"now":"{NOW}"}}}}"#
        )
    }

    #[test]
    fn app_references_need_a_version() {
        let err = load(&with_app("db-client"), AppSet::bundled())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains(r#"img:1 runs app "db-client"; app references must be name@version"#),
            "{err}"
        );
    }

    #[test]
    fn every_referenced_app_must_be_supplied() {
        let err = load_empty(&with_app("db-client@0.1.0")).err().unwrap().to_string();
        assert!(
            err.contains("img:1 runs app db-client@0.1.0, which was not supplied"),
            "{err}"
        );
        assert!(load(&with_app("db-client@0.1.0"), AppSet::bundled()).is_ok());
    }

    #[test]
    fn required_apps_lists_each_reference_once() {
        assert_eq!(
            required_apps(&with_app("db-client@0.1.0")).unwrap(),
            ["db-client@0.1.0"]
        );
        assert!(required_apps("{}").unwrap().is_empty());
        assert!(required_apps("nope").is_err());
    }

    #[test]
    fn pods_are_optional() {
        let cluster = load_empty(&scenario(&[])).unwrap();
        assert!(cluster.store.list("Pod", "default").unwrap().is_empty());
    }

    #[test]
    fn rejects_pod_without_name() {
        let err = load_empty(&scenario(&[pod(r#""namespace":"x""#)])).err().unwrap();
        assert!(matches!(err, ScenarioError::MissingName { index: 0, .. }), "{err}");
    }

    #[test]
    fn rejects_duplicate_pods() {
        let err = load_empty(&scenario(&[pod(r#""name":"a""#), pod(r#""name":"a""#)]))
            .err()
            .unwrap();
        assert!(
            matches!(err, ScenarioError::Store(StoreError::AlreadyExists { .. })),
            "{err}"
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(load_empty("{nope").err().unwrap(), ScenarioError::Parse(_)));
        assert!(matches!(
            load_empty(r#"{"cluster":{}}"#).err().unwrap(),
            ScenarioError::Parse(_)
        ));
    }
}
