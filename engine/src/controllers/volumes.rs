//! A pod's volumes: what each one holds, how a container sees it, and how it changes.
//!
//! Supported sources are `emptyDir` (scratch space shared by the pod's containers, living and
//! dying with the pod), `configMap` and `secret` (read-only files from an object). Anything else
//! (a persistent volume claim, a host path, ...) keeps the pod in `ContainerCreating` with a
//! `FailedMount` event, as a volume the node cannot mount does.
//!
//! The volumes' contents over time are a [`Timeline`]: the state when the pod's volumes were set
//! up, plus time-stamped events. ConfigMap and Secret volumes are refreshed by the kubelet about a
//! minute after the object changes; containers add events by writing to scratch space.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Container, Pod};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apps::vfs::{Files, Volumes};
use crate::apps::{Mount, Timeline, VolEvent};
use crate::shell::base64_decode;
use crate::store::{ClusterStore, StoreError};

/// How long after an object changes the kubelet's copy in the volume does (sync period plus
/// the cache it reads through).
pub const SYNC_SECS: i64 = 60;

/// Runtime-row name for a pod's volume state.
pub const ROW: &str = "#volumes";

/// What a volume is made of.
enum Source {
    EmptyDir,
    ConfigMap(Object),
    Secret(Object),
    /// A source this terminal does not simulate (the field name that was set).
    Unsupported(String),
}

struct Object {
    name: String,
    optional: bool,
    /// `items`: only these keys, at these paths.
    items: Option<Vec<(String, String)>>,
}

fn sources(pod: &Pod) -> Vec<(String, Source)> {
    let volumes = pod.spec.as_ref().and_then(|s| s.volumes.as_ref());
    volumes
        .into_iter()
        .flatten()
        .map(|volume| {
            let items = |items: &Option<Vec<k8s_openapi::api::core::v1::KeyToPath>>| {
                items
                    .as_ref()
                    .map(|list| list.iter().map(|i| (i.key.clone(), i.path.clone())).collect())
            };
            let source = if volume.empty_dir.is_some() {
                Source::EmptyDir
            } else if let Some(cm) = &volume.config_map {
                Source::ConfigMap(Object {
                    name: cm.name.clone(),
                    optional: cm.optional.unwrap_or(false),
                    items: items(&cm.items),
                })
            } else if let Some(secret) = &volume.secret {
                Source::Secret(Object {
                    name: secret.secret_name.clone().unwrap_or_default(),
                    optional: secret.optional.unwrap_or(false),
                    items: items(&secret.items),
                })
            } else {
                let set = serde_json::to_value(volume)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .into_iter()
                    .flatten()
                    .map(|(k, _)| k)
                    .find(|k| k != "name")
                    .unwrap_or_else(|| "(none)".into());
                Source::Unsupported(set)
            };
            (volume.name.clone(), source)
        })
        .collect()
}

/// A ConfigMap's text values, or a Secret's decoded exactly.
fn object_files(object: &Value, secret: bool) -> Files {
    object["data"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let text = value.as_str()?;
            let text = if secret {
                String::from_utf8_lossy(&base64_decode(text)?).into_owned()
            } else {
                text.to_string()
            };
            Some((key.clone(), text))
        })
        .collect()
}

/// The files a ConfigMap or Secret volume shows. `Err` is why it cannot be mounted;
/// `Ok(None)` is an optional object that does not exist.
fn volume_files(
    store: &ClusterStore,
    namespace: &str,
    object: &Object,
    secret: bool,
) -> Result<Result<Files, String>, StoreError> {
    let (kind, noun) = if secret {
        ("Secret", "secret")
    } else {
        ("ConfigMap", "configmap")
    };
    let Some(found) = store.get(kind, namespace, &object.name)? else {
        return Ok(if object.optional {
            Ok(Files::new())
        } else {
            Err(format!("{noun} \"{}\" not found", object.name))
        });
    };
    let all = object_files(&found, secret);
    let Some(items) = &object.items else {
        return Ok(Ok(all));
    };
    let mut chosen = Files::new();
    for (key, path) in items {
        match all.get(key) {
            Some(text) => {
                chosen.insert(path.clone(), text.clone());
            }
            None if object.optional => {}
            None => {
                return Ok(Err(format!(
                    "{noun} references non-existent {} key: {key}",
                    if secret { "secret" } else { "config" }
                )))
            }
        }
    }
    Ok(Ok(chosen))
}

/// The mounts of one container.
pub fn mounts_for(pod: &Pod, container: &Container) -> Vec<Mount> {
    let sources = sources(pod);
    container
        .volume_mounts
        .iter()
        .flatten()
        .filter_map(|mount| {
            let (_, source) = sources.iter().find(|(name, _)| *name == mount.name)?;
            let readonly_source = matches!(source, Source::ConfigMap(_) | Source::Secret(_));
            let sub_path = mount.sub_path.clone().unwrap_or_default();
            Some(Mount {
                path: mount.mount_path.trim_end_matches('/').to_string(),
                volume: mount.name.clone(),
                // A ConfigMap or Secret is a bind of a file that is swapped on refresh; a
                // `subPath` of one therefore stays as it was at the start.
                frozen: readonly_source && !sub_path.is_empty(),
                read_only: readonly_source || mount.read_only.unwrap_or(false),
                sub_path,
            })
        })
        .collect()
}

/// The volume state of a pod, kept with its other kubelet state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VolumeState {
    /// All the pod's volumes are set up.
    pub ready: bool,
    /// The contents when they were set up.
    pub base: Volumes,
    /// Changes since: ConfigMap and Secret refreshes, and the writes of containers whose run
    /// has been replaced by a later one.
    pub events: Vec<VolEvent>,
    /// For ConfigMap and Secret volumes, the `resourceVersion` the contents match.
    pub synced: BTreeMap<String, String>,
}

impl VolumeState {
    /// Set the volumes up: `Err` is the `FailedMount` message.
    pub fn initialize(&mut self, store: &ClusterStore, pod: &Pod) -> Result<Result<(), String>, StoreError> {
        let namespace = pod.metadata.namespace.clone().unwrap_or_default();
        let mut base = Volumes::new();
        let mut synced = BTreeMap::new();
        for (name, source) in sources(pod) {
            let message = |why: String| format!("MountVolume.SetUp failed for volume \"{name}\" : {why}");
            let (object, secret) = match &source {
                Source::EmptyDir => {
                    base.insert(name, Files::new());
                    continue;
                }
                Source::Unsupported(kind) => {
                    return Ok(Err(message(format!(
                        "volume type {kind} is not available in this terminal"
                    ))))
                }
                Source::ConfigMap(object) => (object, false),
                Source::Secret(object) => (object, true),
            };
            match volume_files(store, &namespace, object, secret)? {
                Ok(files) => {
                    let kind = if secret { "Secret" } else { "ConfigMap" };
                    if let Some(found) = store.get(kind, &namespace, &object.name)? {
                        synced.insert(name.clone(), version(&found));
                    }
                    base.insert(name, files);
                }
                Err(why) => return Ok(Err(message(why))),
            }
        }
        self.base = base;
        self.synced = synced;
        self.ready = true;
        Ok(Ok(()))
    }

    /// The contents of a volume after every change recorded so far.
    fn latest(&self, volume: &str) -> Files {
        let mut state = self.base.clone();
        for event in self.events.iter().filter(|e| e.volume == volume) {
            match (&event.content, event.append) {
                (Some(text), true) => state
                    .entry(volume.into())
                    .or_default()
                    .entry(event.path.clone())
                    .or_default()
                    .push_str(text),
                (Some(text), false) => {
                    state
                        .entry(volume.into())
                        .or_default()
                        .insert(event.path.clone(), text.clone());
                }
                (None, _) => {
                    state.entry(volume.into()).or_default().remove(&event.path);
                }
            }
        }
        state.remove(volume).unwrap_or_default()
    }

    /// Notice ConfigMaps and Secrets that changed since the volume last matched them, and
    /// schedule the refresh. Returns whether anything was scheduled.
    pub fn observe(&mut self, store: &ClusterStore, pod: &Pod, now: i64) -> Result<bool, StoreError> {
        let namespace = pod.metadata.namespace.clone().unwrap_or_default();
        let mut changed = false;
        for (name, source) in sources(pod) {
            let (object, secret) = match &source {
                Source::ConfigMap(object) => (object, false),
                Source::Secret(object) => (object, true),
                _ => continue,
            };
            let kind = if secret { "Secret" } else { "ConfigMap" };
            // A deleted object leaves the volume as it was.
            let Some(found) = store.get(kind, &namespace, &object.name)? else {
                continue;
            };
            let current = version(&found);
            if self.synced.get(&name) == Some(&current) {
                continue;
            }
            let Ok(new) = volume_files(store, &namespace, object, secret)? else {
                continue;
            };
            let old = self.latest(&name);
            let at = now + SYNC_SECS;
            for (path, text) in &new {
                if old.get(path) != Some(text) {
                    self.events.push(VolEvent {
                        at,
                        volume: name.clone(),
                        path: path.clone(),
                        content: Some(text.clone()),
                        append: false,
                    });
                }
            }
            for path in old.keys().filter(|p| !new.contains_key(*p)) {
                self.events.push(VolEvent {
                    at,
                    volume: name.clone(),
                    path: path.clone(),
                    content: None,
                    append: false,
                });
            }
            self.synced.insert(name, current);
            changed = true;
        }
        Ok(changed)
    }

    /// What a container's run sees: this pod's volumes, plus what its siblings write.
    pub fn timeline(&self, peer_writes: &[VolEvent]) -> Timeline {
        let mut events = self.events.clone();
        events.extend_from_slice(peer_writes);
        Timeline {
            base: self.base.clone(),
            events,
        }
    }
}

fn version(object: &Value) -> String {
    object["metadata"]["resourceVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(volumes: Value, mounts: Value) -> Pod {
        serde_json::from_value(json!({
            "metadata": {"name": "p", "namespace": "default"},
            "spec": {"volumes": volumes, "containers": [{"name": "c", "image": "x", "volumeMounts": mounts}]}
        }))
        .unwrap()
    }

    fn store_with(objects: &[Value]) -> ClusterStore {
        let store = ClusterStore::new().unwrap();
        for object in objects {
            let kind = object["kind"].as_str().unwrap();
            store
                .insert(
                    kind,
                    "v1",
                    "default",
                    object["metadata"]["name"].as_str().unwrap(),
                    object,
                )
                .unwrap();
        }
        store
    }

    fn config_map(name: &str, data: Value) -> Value {
        json!({"kind": "ConfigMap", "metadata": {"name": name}, "data": data})
    }

    #[test]
    fn mounts_say_where_and_how() {
        let p = pod(
            json!([{"name": "cfg", "configMap": {"name": "c"}}, {"name": "scratch", "emptyDir": {}}]),
            json!([
                {"name": "cfg", "mountPath": "/etc/config/"},
                {"name": "cfg", "mountPath": "/app/mode", "subPath": "mode"},
                {"name": "scratch", "mountPath": "/data"},
                {"name": "scratch", "mountPath": "/ro", "readOnly": true},
                {"name": "nope", "mountPath": "/x"}
            ]),
        );
        let mounts = mounts_for(&p, &p.spec.as_ref().unwrap().containers[0]);
        assert_eq!(mounts.len(), 4, "a mount of an unknown volume is dropped");
        assert_eq!(
            (mounts[0].path.as_str(), mounts[0].read_only, mounts[0].frozen),
            ("/etc/config", true, false)
        );
        assert_eq!((mounts[1].sub_path.as_str(), mounts[1].frozen), ("mode", true));
        assert_eq!((mounts[2].read_only, mounts[2].frozen), (false, false));
        assert!(mounts[3].read_only);
    }

    #[test]
    fn setup_gathers_files_and_names_what_cannot_be_mounted() {
        let store = store_with(&[
            config_map("c", json!({"a": "1", "b": "2"})),
            json!({"kind": "Secret", "metadata": {"name": "s"}, "data": {"pw": "cGFzcwo="}}),
        ]);
        let mut state = VolumeState::default();
        let p = pod(
            json!([
                {"name": "cfg", "configMap": {"name": "c", "items": [{"key": "a", "path": "sub/renamed"}]}},
                {"name": "sec", "secret": {"secretName": "s"}},
                {"name": "scratch", "emptyDir": {}}
            ]),
            json!([]),
        );
        assert_eq!(state.initialize(&store, &p).unwrap(), Ok(()));
        assert!(state.ready);
        assert_eq!(
            state.base["cfg"],
            Files::from([("sub/renamed".to_string(), "1".to_string())])
        );
        assert_eq!(state.base["sec"]["pw"], "pass\n", "a secret is decoded exactly");
        assert!(state.base["scratch"].is_empty());

        let mut state = VolumeState::default();
        let missing = pod(json!([{"name": "cfg", "configMap": {"name": "gone"}}]), json!([]));
        assert_eq!(
            state.initialize(&store, &missing).unwrap(),
            Err("MountVolume.SetUp failed for volume \"cfg\" : configmap \"gone\" not found".into())
        );
        assert!(!state.ready);
        let optional = pod(
            json!([{"name": "cfg", "configMap": {"name": "gone", "optional": true}}]),
            json!([]),
        );
        assert_eq!(state.initialize(&store, &optional).unwrap(), Ok(()));
        let bad_key = pod(
            json!([{"name": "cfg", "configMap": {"name": "c", "items": [{"key": "zzz", "path": "z"}]}}]),
            json!([]),
        );
        assert_eq!(
            VolumeState::default().initialize(&store, &bad_key).unwrap(),
            Err(
                "MountVolume.SetUp failed for volume \"cfg\" : configmap references non-existent config key: zzz"
                    .into()
            )
        );
        let claim = pod(
            json!([{"name": "data", "persistentVolumeClaim": {"claimName": "pvc"}}]),
            json!([]),
        );
        assert_eq!(
            VolumeState::default().initialize(&store, &claim).unwrap(),
            Err("MountVolume.SetUp failed for volume \"data\" : volume type persistentVolumeClaim is not available in this terminal".into())
        );
        let secret_missing = pod(json!([{"name": "sec", "secret": {"secretName": "nope"}}]), json!([]));
        assert!(VolumeState::default()
            .initialize(&store, &secret_missing)
            .unwrap()
            .unwrap_err()
            .ends_with("secret \"nope\" not found"));
    }

    #[test]
    fn a_changed_config_map_is_scheduled_to_appear_after_the_sync_delay() {
        let store = store_with(&[config_map("c", json!({"a": "1", "b": "2"}))]);
        let p = pod(json!([{"name": "cfg", "configMap": {"name": "c"}}]), json!([]));
        let mut state = VolumeState::default();
        state.initialize(&store, &p).unwrap().unwrap();
        assert!(!state.observe(&store, &p, 100).unwrap(), "nothing changed");

        let mut changed = store.get("ConfigMap", "default", "c").unwrap().unwrap();
        changed["data"] = json!({"a": "1", "b": "two", "c": "3"});
        store.update("ConfigMap", "default", "c", &changed).unwrap();
        assert!(state.observe(&store, &p, 100).unwrap());
        let mut events: Vec<_> = state
            .events
            .iter()
            .map(|e| (e.at, e.path.as_str(), e.content.as_deref()))
            .collect();
        events.sort();
        assert_eq!(
            events,
            [(160, "b", Some("two")), (160, "c", Some("3"))],
            "only what differs, 60 s later"
        );
        assert!(!state.observe(&store, &p, 101).unwrap(), "seen once");

        // A key that goes away is removed from the volume.
        changed["data"] = json!({"a": "1"});
        store.update("ConfigMap", "default", "c", &changed).unwrap();
        state.observe(&store, &p, 200).unwrap();
        assert!(state
            .events
            .iter()
            .any(|e| e.at == 260 && e.path == "b" && e.content.is_none()));
        // And deleting the ConfigMap leaves the volume as it is.
        store.delete("ConfigMap", "default", "c").unwrap();
        assert!(!state.observe(&store, &p, 300).unwrap());
    }

    #[test]
    fn timelines_add_the_siblings_writes() {
        let mut state = VolumeState::default();
        state.base.insert("scratch".into(), Files::new());
        let peer = VolEvent {
            at: 5,
            volume: "scratch".into(),
            path: "log".into(),
            content: Some("x".into()),
            append: false,
        };
        let timeline = state.timeline(std::slice::from_ref(&peer));
        assert_eq!(timeline.events, [peer]);
        assert!(state.events.is_empty(), "the state itself is not changed");
    }
}
