//! SQLite-backed cluster state store: the simulated API server + etcd.
//!
//! One generic table keyed by (kind, namespace, name) holds each resource's manifest as JSON,
//! so supporting a new resource kind needs no schema change. Typed access (e.g. `Pod`) happens
//! in callers via serde. Two side tables hold simulation bookkeeping that is not part of any
//! manifest: container logs and kubelet runtime state.

use std::cell::Cell;
use std::fmt;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde_json::Value;

use crate::names;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    AlreadyExists {
        kind: String,
        namespace: String,
        name: String,
    },
    NotFound {
        kind: String,
        namespace: String,
        name: String,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "state store error: {e}"),
            StoreError::Json(e) => write!(f, "invalid stored manifest: {e}"),
            StoreError::AlreadyExists { kind, namespace, name } => {
                write!(f, "{kind} \"{name}\" already exists in namespace \"{namespace}\"")
            }
            StoreError::NotFound { kind, namespace, name } => {
                write!(f, "{kind} \"{name}\" not found in namespace \"{namespace}\"")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

/// One line of container output, stamped with simulated epoch seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub ts: i64,
    pub line: String,
}

pub struct ClusterStore {
    conn: Connection,
    /// Monotonic counter for uids and resourceVersions; keeps everything deterministic.
    counter: Cell<u64>,
    seed: Cell<u32>,
}

impl ClusterStore {
    pub fn new() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE resources (
                kind        TEXT NOT NULL,
                api_version TEXT NOT NULL,
                namespace   TEXT NOT NULL,
                name        TEXT NOT NULL,
                manifest    TEXT NOT NULL,
                PRIMARY KEY (kind, namespace, name)
            );
            CREATE TABLE logs (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                namespace TEXT NOT NULL,
                pod       TEXT NOT NULL,
                container TEXT NOT NULL,
                instance  INTEGER NOT NULL,
                ts        INTEGER NOT NULL,
                line      TEXT NOT NULL
            );
            CREATE INDEX logs_by_container ON logs (namespace, pod, container, instance);
            CREATE TABLE runtime (
                namespace TEXT NOT NULL,
                pod       TEXT NOT NULL,
                container TEXT NOT NULL,
                state     TEXT NOT NULL,
                next_wake INTEGER,
                PRIMARY KEY (namespace, pod, container)
            );
            CREATE INDEX runtime_by_wake ON runtime (next_wake);",
        )?;
        Ok(ClusterStore {
            conn,
            counter: Cell::new(0),
            seed: Cell::new(0),
        })
    }

    pub fn set_seed(&self, seed: u32) {
        self.seed.set(seed);
    }

    pub fn seed(&self) -> u32 {
        self.seed.get()
    }

    /// Next value of the cluster-wide monotonic counter (never repeats).
    pub fn next_id(&self) -> u64 {
        let next = self.counter.get() + 1;
        self.counter.set(next);
        next
    }

    /// Clone `manifest` and stamp API-server metadata: a fresh `resourceVersion` and, when
    /// `assign_uid` and none is present, a `uid`. Manifests without `metadata` are left alone.
    fn stamp(&self, manifest: &Value, assign_uid: bool) -> Value {
        let mut stamped = manifest.clone();
        if let Some(meta) = stamped.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("resourceVersion".into(), Value::String(self.next_id().to_string()));
            let has_uid = meta.get("uid").and_then(Value::as_str).is_some_and(|u| !u.is_empty());
            if assign_uid && !has_uid {
                meta.insert("uid".into(), Value::String(names::uid(self.seed.get(), self.next_id())));
            }
        }
        stamped
    }

    pub fn insert(
        &self,
        kind: &str,
        api_version: &str,
        namespace: &str,
        name: &str,
        manifest: &Value,
    ) -> Result<(), StoreError> {
        let stamped = self.stamp(manifest, true);
        let result = self.conn.execute(
            "INSERT INTO resources (kind, api_version, namespace, name, manifest)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, api_version, namespace, name, serde_json::to_string(&stamped)?],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                Err(StoreError::AlreadyExists {
                    kind: kind.to_string(),
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Replace an existing manifest. Fails with `NotFound` if it does not exist.
    pub fn update(&self, kind: &str, namespace: &str, name: &str, manifest: &Value) -> Result<(), StoreError> {
        let stamped = self.stamp(manifest, false);
        let changed = self.conn.execute(
            "UPDATE resources SET manifest = ?4 WHERE kind = ?1 AND namespace = ?2 AND name = ?3",
            params![kind, namespace, name, serde_json::to_string(&stamped)?],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound {
                kind: kind.to_string(),
                namespace: namespace.to_string(),
                name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Remove a resource; returns whether it existed.
    pub fn delete(&self, kind: &str, namespace: &str, name: &str) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "DELETE FROM resources WHERE kind = ?1 AND namespace = ?2 AND name = ?3",
            params![kind, namespace, name],
        )?;
        Ok(changed > 0)
    }

    pub fn get(&self, kind: &str, namespace: &str, name: &str) -> Result<Option<Value>, StoreError> {
        let manifest: Option<String> = self
            .conn
            .query_row(
                "SELECT manifest FROM resources WHERE kind = ?1 AND namespace = ?2 AND name = ?3",
                params![kind, namespace, name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(manifest.map(|m| serde_json::from_str(&m)).transpose()?)
    }

    /// All resources of `kind` in `namespace`, ordered by name.
    pub fn list(&self, kind: &str, namespace: &str) -> Result<Vec<Value>, StoreError> {
        self.query_manifests(
            "SELECT manifest FROM resources WHERE kind = ?1 AND namespace = ?2 ORDER BY name",
            params![kind, namespace],
        )
    }

    /// All resources of `kind` across namespaces, ordered by namespace then name.
    pub fn list_all(&self, kind: &str) -> Result<Vec<Value>, StoreError> {
        self.query_manifests(
            "SELECT manifest FROM resources WHERE kind = ?1 ORDER BY namespace, name",
            params![kind],
        )
    }

    /// The names of the Namespace objects, sorted.
    pub fn namespaces(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM resources WHERE kind = 'Namespace' ORDER BY name")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn query_manifests(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Value>, StoreError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for manifest in rows {
            out.push(serde_json::from_str(&manifest?)?);
        }
        Ok(out)
    }

    // ---- container logs -------------------------------------------------------------------

    pub fn append_log(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        instance: u32,
        ts: i64,
        line: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO logs (namespace, pod, container, instance, ts, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![namespace, pod, container, instance, ts, line],
        )?;
        Ok(())
    }

    /// Lines written by one run (`instance`) of a container, in write order.
    pub fn read_logs(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        instance: u32,
    ) -> Result<Vec<LogLine>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, line FROM logs
             WHERE namespace = ?1 AND pod = ?2 AND container = ?3 AND instance = ?4 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![namespace, pod, container, instance], |row| {
            Ok(LogLine {
                ts: row.get(0)?,
                line: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---- kubelet runtime state ------------------------------------------------------------

    /// Upsert the runtime state for a container (or pod-level state with container `"*"`).
    /// `next_wake` is the simulated time at which it next needs attention, if any.
    pub fn runtime_put(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        state: &Value,
        next_wake: Option<i64>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO runtime (namespace, pod, container, state, next_wake)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![namespace, pod, container, serde_json::to_string(state)?, next_wake],
        )?;
        Ok(())
    }

    pub fn runtime_get(&self, namespace: &str, pod: &str, container: &str) -> Result<Option<Value>, StoreError> {
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM runtime WHERE namespace = ?1 AND pod = ?2 AND container = ?3",
                params![namespace, pod, container],
                |row| row.get(0),
            )
            .optional()?;
        Ok(state.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// All runtime rows of a pod as (container, state), ordered by container name.
    pub fn runtime_for_pod(&self, namespace: &str, pod: &str) -> Result<Vec<(String, Value)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT container, state FROM runtime WHERE namespace = ?1 AND pod = ?2 ORDER BY container")?;
        let rows = stmt.query_map(params![namespace, pod], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (container, state) = row?;
            out.push((container, serde_json::from_str(&state)?));
        }
        Ok(out)
    }

    /// Earliest simulated time any runtime row needs attention.
    pub fn next_wake(&self) -> Result<Option<i64>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT MIN(next_wake) FROM runtime WHERE next_wake IS NOT NULL",
            [],
            |row| row.get(0),
        )?)
    }

    /// Drop a pod's logs and runtime rows (they disappear with the pod).
    pub fn purge_pod_state(&self, namespace: &str, pod: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM logs WHERE namespace = ?1 AND pod = ?2",
            params![namespace, pod],
        )?;
        self.conn.execute(
            "DELETE FROM runtime WHERE namespace = ?1 AND pod = ?2",
            params![namespace, pod],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store_with_pods() -> ClusterStore {
        let store = ClusterStore::new().unwrap();
        for (ns, name) in [("default", "b"), ("default", "a"), ("kube-system", "c")] {
            store.insert("Pod", "v1", ns, name, &json!({ "name": name })).unwrap();
        }
        store
    }

    #[test]
    fn get_returns_inserted_manifest() {
        let store = store_with_pods();
        assert_eq!(store.get("Pod", "default", "a").unwrap(), Some(json!({ "name": "a" })));
    }

    #[test]
    fn get_missing_is_none() {
        let store = store_with_pods();
        assert_eq!(store.get("Pod", "default", "nope").unwrap(), None);
        assert_eq!(store.get("Pod", "kube-system", "a").unwrap(), None);
        assert_eq!(store.get("Service", "default", "a").unwrap(), None);
    }

    #[test]
    fn list_filters_by_namespace_and_orders_by_name() {
        let store = store_with_pods();
        let names: Vec<_> = store
            .list("Pod", "default")
            .unwrap()
            .into_iter()
            .map(|v| v["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert!(store.list("Pod", "ghost").unwrap().is_empty());
    }

    #[test]
    fn list_all_spans_namespaces_ordered() {
        let store = store_with_pods();
        let names: Vec<_> = store
            .list_all("Pod")
            .unwrap()
            .into_iter()
            .map(|v| v["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert!(store.list_all("Service").unwrap().is_empty());
    }

    #[test]
    fn namespaces_lists_namespace_objects_sorted() {
        let store = store_with_pods();
        assert!(store.namespaces().unwrap().is_empty(), "pods do not make a namespace");
        for name in ["kube-system", "default"] {
            store
                .insert("Namespace", "v1", "", name, &json!({ "name": name }))
                .unwrap();
        }
        assert_eq!(store.namespaces().unwrap(), ["default", "kube-system"]);
        assert!(ClusterStore::new().unwrap().namespaces().unwrap().is_empty());
    }

    #[test]
    fn duplicate_key_is_rejected() {
        let store = store_with_pods();
        let err = store.insert("Pod", "v1", "default", "a", &json!({})).unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists { .. }), "{err}");
    }

    #[test]
    fn update_replaces_and_missing_is_not_found() {
        let store = store_with_pods();
        store
            .update("Pod", "default", "a", &json!({ "name": "a", "v": 2 }))
            .unwrap();
        assert_eq!(store.get("Pod", "default", "a").unwrap().unwrap()["v"], 2);
        let err = store.update("Pod", "default", "nope", &json!({})).unwrap_err();
        assert!(matches!(err, StoreError::NotFound { .. }), "{err}");
    }

    #[test]
    fn delete_reports_whether_it_existed() {
        let store = store_with_pods();
        assert!(store.delete("Pod", "default", "a").unwrap());
        assert!(!store.delete("Pod", "default", "a").unwrap());
        assert_eq!(store.get("Pod", "default", "a").unwrap(), None);
    }

    #[test]
    fn stamps_uid_once_and_bumps_resource_version() {
        let store = ClusterStore::new().unwrap();
        store.set_seed(42);
        store
            .insert("Pod", "v1", "default", "a", &json!({"metadata": {"name": "a"}}))
            .unwrap();
        let first = store.get("Pod", "default", "a").unwrap().unwrap();
        let uid = first["metadata"]["uid"].as_str().unwrap().to_string();
        assert_eq!(uid.len(), 36);
        let rv1: u64 = first["metadata"]["resourceVersion"].as_str().unwrap().parse().unwrap();

        store.update("Pod", "default", "a", &first).unwrap();
        let second = store.get("Pod", "default", "a").unwrap().unwrap();
        assert_eq!(second["metadata"]["uid"], uid.as_str());
        let rv2: u64 = second["metadata"]["resourceVersion"].as_str().unwrap().parse().unwrap();
        assert!(rv2 > rv1);

        store
            .insert("Pod", "v1", "default", "b", &json!({"metadata": {"name": "b"}}))
            .unwrap();
        assert_ne!(
            store.get("Pod", "default", "b").unwrap().unwrap()["metadata"]["uid"],
            uid.as_str()
        );
    }

    #[test]
    fn keeps_authored_uid() {
        let store = ClusterStore::new().unwrap();
        store
            .insert(
                "Pod",
                "v1",
                "default",
                "a",
                &json!({"metadata": {"name": "a", "uid": "mine"}}),
            )
            .unwrap();
        assert_eq!(
            store.get("Pod", "default", "a").unwrap().unwrap()["metadata"]["uid"],
            "mine"
        );
    }

    #[test]
    fn logs_are_kept_per_instance_in_write_order() {
        let store = ClusterStore::new().unwrap();
        store.append_log("default", "p", "c", 0, 10, "first").unwrap();
        store.append_log("default", "p", "c", 0, 11, "second").unwrap();
        store.append_log("default", "p", "c", 1, 20, "other run").unwrap();
        store
            .append_log("default", "p", "other", 0, 10, "other container")
            .unwrap();
        let lines: Vec<_> = store
            .read_logs("default", "p", "c", 0)
            .unwrap()
            .into_iter()
            .map(|l| (l.ts, l.line))
            .collect();
        assert_eq!(lines, [(10, "first".to_string()), (11, "second".to_string())]);
        assert_eq!(store.read_logs("default", "p", "c", 2).unwrap(), []);
    }

    #[test]
    fn runtime_rows_drive_next_wake() {
        let store = ClusterStore::new().unwrap();
        assert_eq!(store.next_wake().unwrap(), None);
        store
            .runtime_put("default", "p", "a", &json!({"s": 1}), Some(50))
            .unwrap();
        store
            .runtime_put("default", "p", "b", &json!({"s": 2}), Some(30))
            .unwrap();
        store.runtime_put("default", "q", "a", &json!({"s": 3}), None).unwrap();
        assert_eq!(store.next_wake().unwrap(), Some(30));

        store.runtime_put("default", "p", "b", &json!({"s": 2}), None).unwrap();
        assert_eq!(store.next_wake().unwrap(), Some(50));
        assert_eq!(store.runtime_get("default", "p", "a").unwrap(), Some(json!({"s": 1})));
        assert_eq!(store.runtime_get("default", "p", "zzz").unwrap(), None);

        let rows = store.runtime_for_pod("default", "p").unwrap();
        assert_eq!(rows.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn purge_removes_only_that_pods_state() {
        let store = ClusterStore::new().unwrap();
        store.append_log("default", "p", "c", 0, 1, "x").unwrap();
        store.append_log("default", "q", "c", 0, 1, "y").unwrap();
        store.runtime_put("default", "p", "c", &json!({}), Some(5)).unwrap();
        store.runtime_put("default", "q", "c", &json!({}), Some(9)).unwrap();
        store.purge_pod_state("default", "p").unwrap();
        assert!(store.read_logs("default", "p", "c", 0).unwrap().is_empty());
        assert_eq!(store.read_logs("default", "q", "c", 0).unwrap().len(), 1);
        assert_eq!(store.runtime_get("default", "p", "c").unwrap(), None);
        assert_eq!(store.next_wake().unwrap(), Some(9));
    }
}
