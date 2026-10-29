//! Scenario-supplied cluster configuration for the control loops: what each container image
//! runs. All of it is plain data in the open-source scenario file (nodes are Node objects in the
//! store, like everything else in the cluster).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::apps::AppSet;

/// Seconds the first start of a container spends pulling its image.
pub const DEFAULT_PULL_SECONDS: i64 = 2;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClusterConfig {
    /// What runs per image reference, e.g. `"registry.example.com/payments-api:2.4.1"`.
    pub images: BTreeMap<String, ImageSpec>,
    /// The apps those images run, supplied by the caller rather than the scenario file.
    #[serde(skip)]
    pub apps: AppSet,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ImageSpec {
    pub pull_seconds: i64,
    /// The app the image runs, as `name@version` (see `crate::apps`); none means a silent
    /// process that never exits.
    pub app: Option<String>,
    /// The app's own configuration, handed to it as-is (JSON).
    pub config: serde_json::Value,
}

impl Default for ImageSpec {
    fn default() -> Self {
        ImageSpec {
            pull_seconds: DEFAULT_PULL_SECONDS,
            app: None,
            config: serde_json::Value::Null,
        }
    }
}

impl ClusterConfig {
    pub fn pull_seconds(&self, image: &str) -> i64 {
        self.images
            .get(image)
            .map_or(DEFAULT_PULL_SECONDS, |i| i.pull_seconds.max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apps_and_defaults() {
        let config: ClusterConfig = serde_json::from_str(
            r#"{
                "images": {
                    "img:1": {"pullSeconds": 5, "app": "script@0.1.0", "config": {"logs": [{"line": "hi"}]}},
                    "img:2": {}
                }
            }"#,
        )
        .unwrap();
        assert_eq!(config.images["img:1"].app.as_deref(), Some("script@0.1.0"));
        assert_eq!(config.images["img:1"].config["logs"][0]["line"], "hi");
        assert!(config.images["img:2"].app.is_none() && config.images["img:2"].config.is_null());
        assert_eq!(config.pull_seconds("img:1"), 5);
        assert_eq!(config.pull_seconds("img:2"), DEFAULT_PULL_SECONDS);
        assert_eq!(config.pull_seconds("unknown"), DEFAULT_PULL_SECONDS);
    }

    #[test]
    fn the_old_rules_format_is_rejected() {
        let err = serde_json::from_str::<ClusterConfig>(r#"{"images": {"img:1": {"rules": []}}}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `rules`"), "{err}");
    }

    #[test]
    fn empty_config_is_valid() {
        let config: ClusterConfig = serde_json::from_str("{}").unwrap();
        assert!(config.images.is_empty());
    }
}
