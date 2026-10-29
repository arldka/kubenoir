//! Cluster events (`kubectl get events`, the Events table of `describe`).
//!
//! Like the real API server, an event repeating with the same object, type, reason and message
//! is aggregated: `count` goes up and `lastTimestamp` moves, instead of a new event appearing.

use serde_json::{json, Value};

use crate::clock;
use crate::store::{ClusterStore, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Normal,
    Warning,
}

impl EventType {
    fn as_str(self) -> &'static str {
        match self {
            EventType::Normal => "Normal",
            EventType::Warning => "Warning",
        }
    }
}

/// The object an event is about.
pub struct Involved<'a> {
    pub kind: &'a str,
    pub api_version: &'a str,
    pub namespace: &'a str,
    pub name: &'a str,
    pub uid: &'a str,
    /// e.g. `spec.containers{app}`
    pub field_path: Option<String>,
}

/// Who reported it (`kubelet` on `worker-1`, `default-scheduler`, ...).
pub struct Source<'a> {
    pub component: &'a str,
    pub host: Option<&'a str>,
}

pub fn record(
    store: &ClusterStore,
    involved: &Involved<'_>,
    event_type: EventType,
    reason: &str,
    message: &str,
    source: &Source<'_>,
    now: i64,
) -> Result<(), StoreError> {
    let type_str = event_type.as_str();
    let existing = store.list("Event", involved.namespace)?.into_iter().find(|e| {
        e["involvedObject"]["kind"] == involved.kind
            && e["involvedObject"]["name"] == involved.name
            && e["involvedObject"]["uid"] == involved.uid
            && e["type"] == type_str
            && e["reason"] == reason
            && e["message"] == message
    });

    if let Some(mut event) = existing {
        let count = event["count"].as_i64().unwrap_or(1) + 1;
        event["count"] = json!(count);
        event["lastTimestamp"] = json!(clock::format(now));
        let name = event["metadata"]["name"].as_str().unwrap_or_default().to_string();
        return store.update("Event", involved.namespace, &name, &event);
    }

    let name = format!("{}.{:x}", involved.name, store.next_id());
    let mut involved_object = json!({
        "apiVersion": involved.api_version,
        "kind": involved.kind,
        "name": involved.name,
        "namespace": involved.namespace,
        "uid": involved.uid,
    });
    if let Some(path) = &involved.field_path {
        involved_object["fieldPath"] = json!(path);
    }
    let mut source_obj = json!({ "component": source.component });
    if let Some(host) = source.host {
        source_obj["host"] = json!(host);
    }
    let ts = clock::format(now);
    let event: Value = json!({
        "apiVersion": "v1",
        "kind": "Event",
        "metadata": { "name": name, "namespace": involved.namespace, "creationTimestamp": ts },
        "involvedObject": involved_object,
        "reason": reason,
        "message": message,
        "source": source_obj,
        "firstTimestamp": ts,
        "lastTimestamp": ts,
        "count": 1,
        "type": type_str,
    });
    store.insert("Event", "v1", involved.namespace, &name, &event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Event;

    fn pod() -> Involved<'static> {
        Involved {
            kind: "Pod",
            api_version: "v1",
            namespace: "default",
            name: "p",
            uid: "u1",
            field_path: Some("spec.containers{app}".into()),
        }
    }

    fn kubelet() -> Source<'static> {
        Source {
            component: "kubelet",
            host: Some("worker-1"),
        }
    }

    #[test]
    fn records_a_valid_core_v1_event() {
        let store = ClusterStore::new().unwrap();
        record(
            &store,
            &pod(),
            EventType::Normal,
            "Started",
            "Started container app",
            &kubelet(),
            1_793_125_800,
        )
        .unwrap();
        let stored = store.list("Event", "default").unwrap();
        assert_eq!(stored.len(), 1);
        let event: Event = serde_json::from_value(stored[0].clone()).unwrap();
        assert_eq!(event.reason.as_deref(), Some("Started"));
        assert_eq!(event.type_.as_deref(), Some("Normal"));
        assert_eq!(event.count, Some(1));
        assert_eq!(
            event.involved_object.field_path.as_deref(),
            Some("spec.containers{app}")
        );
        assert_eq!(event.source.unwrap().host.as_deref(), Some("worker-1"));
    }

    #[test]
    fn repeats_are_aggregated() {
        let store = ClusterStore::new().unwrap();
        for t in [100, 200, 300] {
            record(&store, &pod(), EventType::Warning, "BackOff", "Back-off", &kubelet(), t).unwrap();
        }
        let stored = store.list("Event", "default").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0]["count"], 3);
        assert_eq!(stored[0]["firstTimestamp"], clock::format(100));
        assert_eq!(stored[0]["lastTimestamp"], clock::format(300));
    }

    #[test]
    fn different_reason_message_or_object_are_separate_events() {
        let store = ClusterStore::new().unwrap();
        record(&store, &pod(), EventType::Normal, "Started", "m", &kubelet(), 1).unwrap();
        record(&store, &pod(), EventType::Normal, "Created", "m", &kubelet(), 1).unwrap();
        record(&store, &pod(), EventType::Normal, "Started", "other", &kubelet(), 1).unwrap();
        let other = Involved {
            name: "q",
            uid: "u2",
            ..pod()
        };
        record(&store, &other, EventType::Normal, "Started", "m", &kubelet(), 1).unwrap();
        assert_eq!(store.list("Event", "default").unwrap().len(), 4);
    }
}
