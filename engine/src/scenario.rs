//! Scenario loading: parses an open-source scenario JSON file, reads the Kubernetes manifests it
//! references and seeds a `ClusterStore` with them.
//!
//! The JSON holds the story and the rules; every cluster object lives in YAML files listed in
//! `cluster.manifests`, written exactly as `kubectl apply` would take them. Unknown top-level
//! fields are ignored so scenario files can carry data the engine does not use (narrative, hints).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde::de::IgnoredAny;
use serde::Deserialize;

use crate::api::{self, Origin};
use crate::apps::{self, AppSet};
use crate::clock;
use crate::controllers::config::{ClusterConfig, ImageSpec};
use crate::judge::{Finding, Rules, Success};
use crate::manifests::{normalize_path, parse_documents, Files};
use crate::names;
use crate::store::{ClusterStore, StoreError};

pub const DEFAULT_NAMESPACE: &str = "default";

/// The scenario format this engine reads.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
struct Scenario {
    #[serde(default, rename = "schemaVersion")]
    schema_version: Option<u32>,
    /// Seeds deterministic uids and generated names.
    #[serde(default)]
    id: String,
    /// What each image runs; see `controllers::config`.
    #[serde(default)]
    images: BTreeMap<String, ImageSpec>,
    /// `"open"` (default): images the scenario does not list pull fine. `"closed"`: only listed
    /// images exist, so any other reference (a mistyped tag) fails to pull.
    #[serde(default, rename = "imageCatalog")]
    image_catalog: ImageCatalog,
    /// What counts as solving the case; see `judge`.
    #[serde(default)]
    success: Option<Success>,
    /// Hidden clues unlocked by investigating; see `judge`.
    #[serde(default)]
    findings: Vec<Finding>,
    /// Text files the player's terminal starts with (`cat`, `kubectl apply -f`), relative to the
    /// scenario file; they appear in the terminal at the same path.
    #[serde(default)]
    files: Vec<String>,
    cluster: ClusterSeed,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ImageCatalog {
    #[default]
    Open,
    Closed,
}

#[derive(Debug, Deserialize)]
struct ClusterSeed {
    /// The simulated "current time", so AGE columns are deterministic.
    now: Time,
    /// When the simulation begins. Objects are created then and the cluster is run forward to
    /// `now`, so restart counts, logs, events and ages come out of the simulation itself.
    #[serde(default, rename = "startedAt")]
    started_at: Option<Time>,
    /// The YAML files holding the cluster's objects, relative to the scenario file.
    #[serde(default)]
    manifests: Vec<String>,
    // Inline objects were schemaVersion 1; their presence is reported, not ignored.
    #[serde(default)]
    nodes: Option<IgnoredAny>,
    #[serde(default)]
    pods: Option<IgnoredAny>,
    #[serde(default)]
    deployments: Option<IgnoredAny>,
}

#[derive(Debug)]
pub enum ScenarioError {
    Parse(serde_json::Error),
    /// Wrong or missing `schemaVersion`, or fields of an older format.
    Format(String),
    /// A `cluster.manifests` entry that is not a safe relative path or was not supplied.
    ManifestRef {
        reference: String,
        reason: String,
    },
    /// A document of a manifest file that does not parse or is refused by the API server.
    Manifest {
        file: String,
        document: usize,
        reason: String,
    },
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
            ScenarioError::Format(reason) => write!(f, "invalid scenario: {reason}"),
            ScenarioError::ManifestRef { reference, reason } => {
                write!(f, "invalid scenario: file reference \"{reference}\" {reason}")
            }
            ScenarioError::Manifest { file, document, reason } => {
                write!(f, "invalid scenario: {file}, document {document}: {reason}")
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
    /// The files the player's terminal starts with, by path.
    pub files: Files,
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

/// The files a scenario references (`cluster.manifests` and `files`), each checked to be a safe
/// path inside the scenario's folder: what the caller must read or fetch and pass to `load`.
pub fn required_files(json: &str) -> Result<Vec<String>, ScenarioError> {
    #[derive(Deserialize)]
    struct Refs {
        #[serde(default)]
        cluster: Option<RefsCluster>,
        #[serde(default)]
        files: Vec<String>,
    }
    #[derive(Deserialize)]
    struct RefsCluster {
        #[serde(default)]
        manifests: Vec<String>,
    }
    let parsed: Refs = serde_json::from_str(json).map_err(ScenarioError::Parse)?;
    let mut refs = parsed.cluster.map(|c| c.manifests).unwrap_or_default();
    for reference in &refs {
        check_reference(reference, true)?;
    }
    for reference in &parsed.files {
        check_reference(reference, false)?;
    }
    refs.extend(parsed.files);
    Ok(refs)
}

/// A file reference must stay inside the scenario folder: relative, no `..`, plain names.
/// Manifests (`manifest`) must also be YAML or JSON files.
fn check_reference(reference: &str, manifest: bool) -> Result<(), ScenarioError> {
    let bad = |reason: &str| {
        Err(ScenarioError::ManifestRef {
            reference: reference.to_string(),
            reason: reason.to_string(),
        })
    };
    let path = normalize_path(reference);
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return bad("must be a relative path inside the scenario folder");
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return bad("must be a relative path inside the scenario folder");
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return bad("may only use A-Z a-z 0-9 . _ - in its path");
        }
    }
    if manifest && !(path.ends_with(".yaml") || path.ends_with(".yml") || path.ends_with(".json")) {
        return bad("must be a .yaml, .yml or .json file");
    }
    Ok(())
}

fn check_format(scenario: &Scenario) -> Result<(), ScenarioError> {
    match scenario.schema_version {
        Some(SCHEMA_VERSION) => {}
        Some(1) => {
            return Err(ScenarioError::Format(format!(
                "schemaVersion 1 is no longer read; move cluster.nodes, cluster.pods and cluster.deployments into YAML files listed in cluster.manifests and set schemaVersion {SCHEMA_VERSION} (see docs/scenario-format.md)"
            )))
        }
        Some(other) => {
            return Err(ScenarioError::Format(format!(
                "unsupported schemaVersion {other}; expected {SCHEMA_VERSION}"
            )))
        }
        None => {
            return Err(ScenarioError::Format(format!(
                "schemaVersion is missing; expected {SCHEMA_VERSION}"
            )))
        }
    }
    let cluster = &scenario.cluster;
    let inline: Vec<&str> = [
        (cluster.nodes.is_some(), "cluster.nodes"),
        (cluster.pods.is_some(), "cluster.pods"),
        (cluster.deployments.is_some(), "cluster.deployments"),
    ]
    .into_iter()
    .filter_map(|(present, field)| present.then_some(field))
    .collect();
    if !inline.is_empty() {
        return Err(ScenarioError::Format(format!(
            "{} must be YAML files listed in cluster.manifests",
            inline.join(", ")
        )));
    }
    Ok(())
}

/// Build the cluster a scenario describes. `files` holds the text of every manifest it
/// references (see `required_files`), keyed by the reference; `apps` every app its images run.
pub fn load(json: &str, files: &Files, apps: AppSet) -> Result<Cluster, ScenarioError> {
    let scenario: Scenario = serde_json::from_str(json).map_err(ScenarioError::Parse)?;
    check_format(&scenario)?;
    // Every case gets the same fifteen minutes; a scenario cannot ask for another amount.
    if let Ok(raw) = serde_json::from_str::<serde_json::Value>(json) {
        if let Some(key) = ["timeLimitMinutes", "timeLimitSeconds", "timeLimit"]
            .into_iter()
            .find(|k| raw.get(k).is_some())
        {
            return Err(ScenarioError::Format(format!(
                "{key} is not allowed: every case has the same fixed limit of 15 minutes"
            )));
        }
    }
    for (image, spec) in &scenario.images {
        let Some(app) = spec.app.as_deref() else { continue };
        if !apps::is_reference(app) {
            return Err(ScenarioError::Image(format!(
                "{image} runs app {app:?}; app references must be name@version, e.g. db-client@0.2.0"
            )));
        }
        if !apps.contains(app) {
            return Err(ScenarioError::Image(format!(
                "{image} runs app {app}, which was not supplied (is it in the app registry?)"
            )));
        }
    }

    let now = scenario.cluster.now;
    let started_at = scenario.cluster.started_at.unwrap_or_else(|| now.clone());
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

    let store = ClusterStore::new()?;
    store.set_seed(names::seed_for(&scenario.id));
    let start = clock::secs(&started_at);
    api::seed_system_namespaces(&store, start)?;
    seed_manifests(&store, &scenario.cluster.manifests, files, start)?;
    let mut player_files = Files::new();
    for reference in &scenario.files {
        check_reference(reference, false)?;
        let text = files
            .get(&normalize_path(reference))
            .ok_or_else(|| ScenarioError::ManifestRef {
                reference: reference.clone(),
                reason: "was not supplied (is the file next to scenario.json?)".into(),
            })?;
        player_files.insert(normalize_path(reference), text.clone());
    }

    let rules = Rules {
        success: scenario.success,
        findings: scenario.findings,
    };
    rules.validate().map_err(ScenarioError::Rules)?;

    let config = ClusterConfig {
        images: scenario.images,
        apps,
        closed_catalog: scenario.image_catalog == ImageCatalog::Closed,
    };
    Ok(Cluster {
        store,
        config,
        rules,
        started_at,
        files: player_files,
        now,
    })
}

/// Create every object of every referenced file, through the same write path as `kubectl
/// apply` but keeping what the author wrote (status, creation time). Namespaces come first, so
/// a file may hold objects of a namespace it declares further down.
fn seed_manifests(store: &ClusterStore, refs: &[String], files: &Files, start: i64) -> Result<(), ScenarioError> {
    let mut documents = Vec::new();
    for reference in refs {
        check_reference(reference, true)?;
        let text = files
            .get(&normalize_path(reference))
            .ok_or_else(|| ScenarioError::ManifestRef {
                reference: reference.clone(),
                reason: "was not supplied (is the file next to scenario.json?)".into(),
            })?;
        let objects = parse_documents(text).map_err(|reason| ScenarioError::Manifest {
            file: reference.clone(),
            document: 1,
            reason: format!("not valid YAML: {reason}"),
        })?;
        documents.extend(
            objects
                .into_iter()
                .enumerate()
                .map(|(i, object)| (reference.as_str(), i + 1, object)),
        );
    }
    documents.sort_by_key(|(_, _, object)| object["kind"] != "Namespace");
    for (file, document, object) in documents {
        api::create(store, &object, DEFAULT_NAMESPACE, start, Origin::Authored).map_err(|e| match e {
            api::ApiError::Store(e) => ScenarioError::Store(e),
            other => ScenarioError::Manifest {
                file: file.to_string(),
                document,
                reason: other.to_string(),
            },
        })?;
    }
    Ok(())
}

/// Test fixtures in the shape older scenarios had (`cluster.nodes` as names, `cluster.pods` and
/// `cluster.deployments` inline), turned into a current scenario and its manifest file.
#[cfg(test)]
pub(crate) mod inline {
    use serde_json::{json, Value};

    use super::*;

    pub const FILE: &str = "manifests/cluster.yaml";

    pub fn convert(json: &str) -> (String, Files) {
        let mut scenario: Value = serde_json::from_str(json).expect("fixture is JSON");
        let cluster = scenario["cluster"].as_object_mut().expect("fixture has a cluster");
        let mut documents: Vec<Value> = Vec::new();
        if let Some(Value::Array(nodes)) = cluster.remove("nodes") {
            for node in nodes {
                documents.push(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": node}}));
            }
        }
        for field in ["pods", "deployments"] {
            if let Some(Value::Array(objects)) = cluster.remove(field) {
                documents.extend(objects);
            }
        }
        cluster.insert("manifests".into(), json!([FILE]));
        scenario["schemaVersion"] = json!(SCHEMA_VERSION);
        let text = documents
            .iter()
            .map(|d| serde_json::to_string(d).expect("serializable"))
            .collect::<Vec<_>>()
            .join("\n---\n");
        (scenario.to_string(), Files::from([(FILE.to_string(), text)]))
    }

    /// Load an inline-shaped fixture.
    pub fn load(json: &str, apps: AppSet) -> Result<Cluster, ScenarioError> {
        let (scenario, files) = convert(json);
        super::load(&scenario, &files, apps)
    }

    /// An engine for one of the repository's scenarios (`scenarios/<id>/`), with its manifests.
    pub fn repo(id: &str) -> crate::sim::Engine {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenarios")
            .join(id);
        let json = std::fs::read_to_string(folder.join("scenario.json")).expect("scenario exists");
        let files: Files = required_files(&json)
            .expect("valid references")
            .into_iter()
            .map(|r| {
                let text = std::fs::read_to_string(folder.join(&r)).expect("manifest exists");
                (r, text)
            })
            .collect();
        crate::sim::Engine::from_scenario(&json, &files, AppSet::bundled()).expect("scenario loads")
    }

    /// An engine for an inline-shaped fixture.
    pub fn engine(json: &str) -> crate::sim::Engine {
        let (scenario, files) = convert(json);
        crate::sim::Engine::from_scenario(&scenario, &files, AppSet::bundled()).expect("fixture loads")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-27T18:30:00Z";

    fn scenario_with(manifests: &[&str]) -> String {
        format!(
            r#"{{"schemaVersion":2,"id":"t","cluster":{{"now":"{NOW}","manifests":{}}}}}"#,
            serde_json::to_string(manifests).unwrap()
        )
    }

    fn files(entries: &[(&str, &str)]) -> Files {
        entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const PODS: &str = "\
apiVersion: v1
kind: Pod
metadata:
  name: a
spec:
  containers: [{name: c, image: nginx:1.27}]
---
apiVersion: v1
kind: Pod
metadata:
  name: b
  namespace: prod
spec:
  containers: [{name: c, image: nginx:1.27}]
---
apiVersion: v1
kind: Namespace
metadata:
  name: prod
";

    #[test]
    fn loads_manifests_and_defaults_namespace() {
        let cluster = load(
            &scenario_with(&["manifests/pods.yaml"]),
            &files(&[("manifests/pods.yaml", PODS)]),
            AppSet::default(),
        )
        .unwrap();
        let a = cluster.store.get("Pod", "default", "a").unwrap().unwrap();
        assert_eq!(a["metadata"]["namespace"], "default");
        assert_eq!(a["spec"]["restartPolicy"], "Always", "server-side defaults apply");
        assert!(cluster.store.get("Pod", "prod", "b").unwrap().is_some());
        assert!(cluster.store.get("Namespace", "", "kube-system").unwrap().is_some());
    }

    #[test]
    fn authored_status_is_kept() {
        let static_pod = "\
apiVersion: v1
kind: Pod
metadata: {name: a, creationTimestamp: '2026-10-24T18:30:00Z'}
spec:
  containers: [{name: c, image: nginx:1.27}]
status: {phase: Running}
";
        let cluster = load(
            &scenario_with(&["a.yaml"]),
            &files(&[("a.yaml", static_pod)]),
            AppSet::default(),
        )
        .unwrap();
        let a = cluster.store.get("Pod", "default", "a").unwrap().unwrap();
        assert_eq!(a["status"]["phase"], "Running");
        assert_eq!(a["metadata"]["creationTimestamp"], "2026-10-24T18:30:00Z");
    }

    #[test]
    fn invalid_documents_name_their_file_and_index() {
        let bad = format!("{PODS}---\napiVersion: v1\nkind: Pod\nmetadata: {{name: c}}\nspec:\n  containers: [{{name: c, imagee: x}}]\n");
        let err = load(
            &scenario_with(&["ok.yaml", "bad.yaml"]),
            &files(&[
                ("ok.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: fine}\n"),
                ("bad.yaml", &bad),
            ]),
            AppSet::default(),
        )
        .err()
        .unwrap()
        .to_string();
        assert_eq!(
            err,
            "invalid scenario: bad.yaml, document 4: Error from server (BadRequest): Pod in version \"v1\" cannot be handled as a Pod: strict decoding error: unknown field \"spec.containers[0].imagee\""
        );

        let err = load(
            &scenario_with(&["y.yaml"]),
            &files(&[("y.yaml", "kind: [")]),
            AppSet::default(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            err.starts_with("invalid scenario: y.yaml, document 1: not valid YAML: "),
            "{err}"
        );
    }

    #[test]
    fn manifest_references_must_be_supplied_and_stay_inside_the_folder() {
        let err = load(&scenario_with(&["missing.yaml"]), &Files::new(), AppSet::default())
            .err()
            .unwrap()
            .to_string();
        assert_eq!(
            err,
            "invalid scenario: file reference \"missing.yaml\" was not supplied (is the file next to scenario.json?)"
        );
        for reference in ["../other/x.yaml", "/etc/x.yaml", "a//b.yaml", "a/./b.yaml", ""] {
            let err = required_files(&scenario_with(&[reference])).err().unwrap().to_string();
            assert!(
                err.contains("must be a relative path inside the scenario folder"),
                "{reference}: {err}"
            );
        }
        assert!(required_files(&scenario_with(&["x.txt"]))
            .err()
            .unwrap()
            .to_string()
            .contains(".yaml"));
        assert!(required_files(&scenario_with(&["a b.yaml"])).is_err());
        assert_eq!(
            required_files(&scenario_with(&["./manifests/a.yaml", "b.yml"])).unwrap(),
            ["./manifests/a.yaml", "b.yml"]
        );
        assert!(required_files(r#"{"cluster":{"now":"x"}}"#).unwrap().is_empty());
    }

    #[test]
    fn a_bad_document_leaves_no_cluster() {
        // Errors are returned before a Cluster exists, so nothing partial reaches the engine.
        let result = load(
            &scenario_with(&["a.yaml"]),
            &files(&[("a.yaml", "apiVersion: v1\nkind: Widget\nmetadata: {name: w}\n")]),
            AppSet::default(),
        );
        assert!(matches!(result, Err(ScenarioError::Manifest { document: 1, .. })));
    }

    #[test]
    fn schema_version_must_be_current_and_inline_objects_are_refused() {
        let err = |json: &str| load(json, &Files::new(), AppSet::default()).err().unwrap().to_string();
        let v1 = format!(r#"{{"schemaVersion":1,"cluster":{{"now":"{NOW}"}}}}"#);
        assert!(err(&v1).contains("schemaVersion 1 is no longer read"), "{}", err(&v1));
        let none = format!(r#"{{"cluster":{{"now":"{NOW}"}}}}"#);
        assert_eq!(err(&none), "invalid scenario: schemaVersion is missing; expected 2");
        let inline = format!(r#"{{"schemaVersion":2,"cluster":{{"now":"{NOW}","nodes":["a"],"pods":[]}}}}"#);
        assert_eq!(
            err(&inline),
            "invalid scenario: cluster.nodes, cluster.pods must be YAML files listed in cluster.manifests"
        );
    }

    fn with_app(app: &str) -> String {
        format!(
            r#"{{"schemaVersion":2,"id":"t","images":{{"img:1":{{"app":"{app}"}},"img:2":{{"app":"{app}"}},"img:3":{{}}}},"cluster":{{"now":"{NOW}"}}}}"#
        )
    }

    #[test]
    fn app_references_need_a_version() {
        let err = load(&with_app("db-client"), &Files::new(), AppSet::bundled())
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
        let err = load(&with_app("db-client@0.2.0"), &Files::new(), AppSet::default())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("img:1 runs app db-client@0.2.0, which was not supplied"),
            "{err}"
        );
        assert!(load(&with_app("db-client@0.2.0"), &Files::new(), AppSet::bundled()).is_ok());
    }

    #[test]
    fn required_apps_lists_each_reference_once() {
        assert_eq!(
            required_apps(&with_app("db-client@0.2.0")).unwrap(),
            ["db-client@0.2.0"]
        );
        assert!(required_apps("{}").unwrap().is_empty());
        assert!(required_apps("nope").is_err());
    }

    #[test]
    fn scenario_files_seed_the_players_terminal_and_are_required() {
        let json = format!(
            r#"{{"schemaVersion":2,"id":"t","files":["notes/case.txt","./todo.txt"],"cluster":{{"now":"{NOW}","manifests":["a.yaml"]}}}}"#
        );
        assert_eq!(
            required_files(&json).unwrap(),
            ["a.yaml", "notes/case.txt", "./todo.txt"]
        );
        let supplied = files(&[("a.yaml", ""), ("notes/case.txt", "hello"), ("todo.txt", "x")]);
        let cluster = load(&json, &supplied, AppSet::default()).unwrap();
        assert_eq!(cluster.files.get("notes/case.txt").map(String::as_str), Some("hello"));
        assert_eq!(cluster.files.get("todo.txt").map(String::as_str), Some("x"));
        assert!(
            !cluster.files.contains_key("a.yaml"),
            "manifests are not terminal files"
        );

        let missing = files(&[("a.yaml", "")]);
        let err = load(&json, &missing, AppSet::default()).err().unwrap().to_string();
        assert_eq!(
            err,
            "invalid scenario: file reference \"notes/case.txt\" was not supplied (is the file next to scenario.json?)"
        );
        let bad = format!(r#"{{"schemaVersion":2,"files":["../x.txt"],"cluster":{{"now":"{NOW}"}}}}"#);
        assert!(required_files(&bad)
            .err()
            .unwrap()
            .to_string()
            .contains("inside the scenario folder"));
    }

    #[test]
    fn manifests_are_optional() {
        let cluster = load(&scenario_with(&[]), &Files::new(), AppSet::default()).unwrap();
        assert!(cluster.store.list("Pod", "default").unwrap().is_empty());
    }

    #[test]
    fn rejects_pod_without_name_and_duplicates() {
        let nameless =
            "apiVersion: v1\nkind: Pod\nmetadata: {namespace: default}\nspec:\n  containers: [{name: c, image: x:1}]\n";
        let err = load(
            &scenario_with(&["a.yaml"]),
            &files(&[("a.yaml", nameless)]),
            AppSet::default(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(err.contains("metadata.name: Required value"), "{err}");

        let twice = "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: a}\n---\napiVersion: v1\nkind: ConfigMap\nmetadata: {name: a}\n";
        let err = load(
            &scenario_with(&["a.yaml"]),
            &files(&[("a.yaml", twice)]),
            AppSet::default(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            err.contains("document 2: Error from server (AlreadyExists): configmaps \"a\" already exists"),
            "{err}"
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            load("{nope", &Files::new(), AppSet::default()).err().unwrap(),
            ScenarioError::Parse(_)
        ));
        assert!(matches!(
            load(r#"{"cluster":{}}"#, &Files::new(), AppSet::default())
                .err()
                .unwrap(),
            ScenarioError::Parse(_)
        ));
    }

    #[test]
    fn inline_fixtures_convert() {
        let cluster = inline::load(
            r#"{"id":"t","cluster":{"now":"2026-10-27T18:30:00Z","nodes":["n1"],"pods":[{"apiVersion":"v1","kind":"Pod","metadata":{"name":"a"},"spec":{"containers":[{"name":"c","image":"x:1"}]}}]}}"#,
            AppSet::default(),
        )
        .unwrap();
        assert!(cluster.store.get("Node", "", "n1").unwrap().is_some());
        assert!(cluster.store.get("Pod", "default", "a").unwrap().is_some());
    }
}
