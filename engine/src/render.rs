//! Output printers for Pods: kubectl-style tables, `-o wide`, `-o name`, `-o json`, `-o yaml`.

use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{ConfigMap, Endpoints, Event, Namespace, Node, Pod, PodTemplateSpec, Secret, Service};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::api::KindInfo;
use crate::clock;
use crate::parser::OutputFormat;

const NONE: &str = "<none>";

#[derive(Serialize)]
struct ItemList<'a> {
    #[serde(rename = "apiVersion")]
    api_version: &'static str,
    items: &'a [Value],
    kind: &'static str,
    metadata: ListMeta,
}

#[derive(Serialize)]
struct ListMeta {
    #[serde(rename = "resourceVersion")]
    resource_version: &'static str,
}

/// How `kubectl get` wants its objects printed.
pub struct RenderOptions<'a> {
    pub format: &'a OutputFormat,
    /// One object fetched by name: `-o json|yaml|jsonpath` use the bare object, not a list.
    pub single: bool,
    /// `-A`: tables get a NAMESPACE column.
    pub all_namespaces: bool,
    pub no_headers: bool,
    /// `--show-labels`: tables end with a LABELS column.
    pub show_labels: bool,
    pub now: &'a Time,
}

/// Render stored manifests of `kind` in the requested format.
///
/// With `single` set (one object fetched by name) `-o json|yaml` prints the bare object,
/// otherwise a `List` wrapper, matching kubectl. Structured output prints the manifests as
/// stored (keys sorted, like kubectl's own output). `Err` is a message for `error: ...`.
pub fn render(info: &KindInfo, items: &[Value], options: &RenderOptions<'_>) -> Result<String, String> {
    let bare = options.single && items.len() == 1;
    Ok(match options.format {
        OutputFormat::Table => table(info, items, false, options.now).print(options),
        OutputFormat::Wide => table(info, items, true, options.now).print(options),
        OutputFormat::Name => items
            .iter()
            .map(|v| {
                format!(
                    "{}/{}",
                    info.name_prefix(),
                    v["metadata"]["name"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        OutputFormat::Json if bare => to_json(&items[0]),
        OutputFormat::Json => to_json(&item_list(items)),
        OutputFormat::Yaml if bare => to_yaml(&items[0]),
        OutputFormat::Yaml => to_yaml(&item_list(items)),
        OutputFormat::JsonPath(template) => {
            let root = if bare {
                items[0].clone()
            } else {
                serde_json::to_value(item_list(items)).map_err(|e| e.to_string())?
            };
            crate::jsonpath::render(template, &root)?
        }
    })
}

fn item_list(items: &[Value]) -> ItemList<'_> {
    ItemList {
        api_version: "v1",
        items,
        kind: "List",
        metadata: ListMeta { resource_version: "" },
    }
}

/// Column headers and rows, with each row's namespace kept aside for `-A`.
struct Table {
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
    namespaces: Vec<String>,
    labels: Vec<String>,
}

impl Table {
    fn new<'a>(
        headers: Vec<&'static str>,
        rows: Vec<Vec<String>>,
        metas: impl Iterator<Item = &'a ObjectMeta>,
    ) -> Table {
        let metas: Vec<&ObjectMeta> = metas.collect();
        Table {
            headers,
            rows,
            namespaces: metas.iter().map(|m| m.namespace.clone().unwrap_or_default()).collect(),
            labels: metas
                .iter()
                .map(|m| {
                    m.labels
                        .iter()
                        .flatten()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect(),
        }
    }

    fn print(mut self, options: &RenderOptions<'_>) -> String {
        if options.show_labels {
            self.headers.push("LABELS");
            for (row, labels) in self.rows.iter_mut().zip(&self.labels) {
                row.push(if labels.is_empty() {
                    "<none>".into()
                } else {
                    labels.clone()
                });
            }
        }
        if options.all_namespaces {
            self.headers.insert(0, "NAMESPACE");
            for (row, namespace) in self.rows.iter_mut().zip(self.namespaces) {
                row.insert(0, namespace);
            }
        }
        if options.no_headers {
            // Columns are sized by the rows alone, and the header line is dropped.
            let blank = vec![""; self.headers.len()];
            return format_table(&blank, &self.rows)
                .split_once('\n')
                .map_or_else(String::new, |(_, rows)| rows.to_string());
        }
        format_table(&self.headers, &self.rows)
    }
}

fn table(info: &KindInfo, items: &[Value], wide: bool, now: &Time) -> Table {
    match info.kind {
        "Pod" => pod_table(&typed::<Pod>(items), wide, now),
        "Event" => event_table(&typed::<Event>(items), wide, now),
        "Deployment" => deployment_table(&typed::<Deployment>(items), wide, now),
        "ReplicaSet" => replicaset_table(&typed::<ReplicaSet>(items), wide, now),
        "ConfigMap" => configmap_table(&typed::<ConfigMap>(items), now),
        "Secret" => secret_table(&typed::<Secret>(items), now),
        "Namespace" => namespace_table(&typed::<Namespace>(items), now),
        "Node" => node_table(&typed::<Node>(items), wide, now),
        "Endpoints" => endpoints_table(&typed::<Endpoints>(items), now),
        "Job" => job_table(&typed::<Job>(items), wide, now),
        "CronJob" => cronjob_table(&typed::<CronJob>(items), wide, now),
        "NetworkPolicy" => policy_table(&typed::<NetworkPolicy>(items), now),
        "EndpointSlice" => slice_table(&typed::<EndpointSlice>(items), now),
        _ => service_table(&typed::<Service>(items), wide, now),
    }
}

fn typed<T: DeserializeOwned>(items: &[Value]) -> Vec<T> {
    items
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

/// kubectl indents JSON with four spaces.
fn to_json<T: Serialize>(value: &T) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    match value.serialize(&mut ser) {
        Ok(()) => String::from_utf8(buf).unwrap_or_default(),
        Err(e) => format!("error: {e}"),
    }
}

fn to_yaml<T: Serialize>(value: &T) -> String {
    match serde_yaml_ng::to_string(value) {
        Ok(s) => s.trim_end().to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn pod_table(pods: &[Pod], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "READY", "STATUS", "RESTARTS", "AGE"];
    if wide {
        headers.extend(["IP", "NODE", "NOMINATED NODE", "READINESS GATES"]);
    }

    let rows: Vec<Vec<String>> = pods
        .iter()
        .map(|pod| {
            let mut row = vec![
                pod.metadata.name.clone().unwrap_or_default(),
                pod_ready(pod),
                pod_status(pod),
                pod_restarts(pod).to_string(),
                pod_age(pod, now),
            ];
            if wide {
                let status = pod.status.as_ref();
                let spec = pod.spec.as_ref();
                row.push(status.and_then(|s| s.pod_ip.clone()).unwrap_or_else(|| NONE.into()));
                row.push(spec.and_then(|s| s.node_name.clone()).unwrap_or_else(|| NONE.into()));
                row.push(
                    status
                        .and_then(|s| s.nominated_node_name.clone())
                        .unwrap_or_else(|| NONE.into()),
                );
                row.push(match spec.and_then(|s| s.readiness_gates.as_ref()) {
                    Some(gates) if !gates.is_empty() => gates
                        .iter()
                        .map(|g| g.condition_type.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    _ => NONE.into(),
                });
            }
            row
        })
        .collect();

    Table::new(headers, rows, pods.iter().map(|p| &p.metadata))
}

/// Render a kubectl-style table: columns padded to the widest cell plus three spaces.
pub(crate) fn format_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    format_table_with_gap(headers, rows, 3)
}

/// Like [`format_table`] with a custom number of spaces between columns.
pub(crate) fn format_table_with_gap(headers: &[&str], rows: &[Vec<String>], gap: usize) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    let render = |cells: Vec<&str>| -> String {
        let last = cells.len() - 1;
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    c.to_string()
                } else {
                    format!("{c:<width$}{}", " ".repeat(gap), width = widths[i])
                }
            })
            .collect()
    };

    let mut lines = vec![render(headers.to_vec())];
    lines.extend(rows.iter().map(|r| render(r.iter().map(String::as_str).collect())));
    lines.join("\n")
}

fn event_secs(time: &Option<Time>) -> Option<i64> {
    time.as_ref().map(clock::secs)
}

/// `LAST SEEN` / `Age` text: time since the last occurrence, plus `(x14 over 46m)` for repeats.
pub(crate) fn event_age(event: &Event, now: i64) -> String {
    let since = |t: &Option<Time>| event_secs(t).map_or_else(|| "<unknown>".to_string(), |t| human_duration(now - t));
    let last = since(&event.last_timestamp);
    match event.count {
        Some(count) if count > 1 => format!("{last} (x{count} over {})", since(&event.first_timestamp)),
        _ => last,
    }
}

fn event_table(events: &[Event], wide: bool, now: &Time) -> Table {
    let now = clock::secs(now);
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| (event_secs(&e.last_timestamp).unwrap_or(0), e.metadata.name.clone()));

    let mut headers = vec!["LAST SEEN", "TYPE", "REASON", "OBJECT"];
    if wide {
        headers.extend(["SUBOBJECT", "SOURCE"]);
    }
    headers.push("MESSAGE");
    if wide {
        headers.extend(["FIRST SEEN", "COUNT", "NAME"]);
    }

    let rows: Vec<Vec<String>> = sorted
        .iter()
        .map(|e| {
            let object = format!(
                "{}/{}",
                e.involved_object.kind.as_deref().unwrap_or_default().to_lowercase(),
                e.involved_object.name.as_deref().unwrap_or_default()
            );
            let mut row = vec![
                event_age(e, now),
                e.type_.clone().unwrap_or_default(),
                e.reason.clone().unwrap_or_default(),
                object,
            ];
            if wide {
                row.push(e.involved_object.field_path.clone().unwrap_or_default());
                let source = e.source.as_ref();
                let component = source.and_then(|s| s.component.clone()).unwrap_or_default();
                row.push(match source.and_then(|s| s.host.clone()) {
                    Some(host) => format!("{component}, {host}"),
                    None => component,
                });
            }
            row.push(e.message.clone().unwrap_or_default());
            if wide {
                row.push(
                    event_secs(&e.first_timestamp).map_or_else(|| "<unknown>".into(), |t| human_duration(now - t)),
                );
                row.push(e.count.unwrap_or(1).to_string());
                row.push(e.metadata.name.clone().unwrap_or_default());
            }
            row
        })
        .collect();

    Table::new(headers, rows, sorted.iter().map(|e| &e.metadata))
}

fn object_age(meta: &ObjectMeta, now: &Time) -> String {
    match &meta.creation_timestamp {
        Some(created) => human_duration(clock::secs(now) - clock::secs(created)),
        None => "<unknown>".into(),
    }
}

/// `k=v,k=v` form of a label selector's `matchLabels`.
fn selector_string(selector: Option<&LabelSelector>) -> String {
    selector
        .and_then(|s| s.match_labels.as_ref())
        .map(|m| m.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","))
        .unwrap_or_default()
}

/// The `CONTAINERS IMAGES SELECTOR` columns of `-o wide` for workload tables.
fn workload_wide(template: Option<&PodTemplateSpec>, selector: Option<&LabelSelector>) -> [String; 3] {
    let containers = template
        .and_then(|t| t.spec.as_ref())
        .map(|s| s.containers.as_slice())
        .unwrap_or_default();
    [
        containers.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(","),
        containers
            .iter()
            .filter_map(|c| c.image.as_deref())
            .collect::<Vec<_>>()
            .join(","),
        selector_string(selector),
    ]
}

fn deployment_table(deployments: &[Deployment], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "READY", "UP-TO-DATE", "AVAILABLE", "AGE"];
    if wide {
        headers.extend(["CONTAINERS", "IMAGES", "SELECTOR"]);
    }
    let rows: Vec<Vec<String>> = deployments
        .iter()
        .map(|d| {
            let status = d.status.as_ref();
            let desired = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
            let mut row = vec![
                d.metadata.name.clone().unwrap_or_default(),
                format!("{}/{desired}", status.and_then(|s| s.ready_replicas).unwrap_or(0)),
                status.and_then(|s| s.updated_replicas).unwrap_or(0).to_string(),
                status.and_then(|s| s.available_replicas).unwrap_or(0).to_string(),
                object_age(&d.metadata, now),
            ];
            if wide {
                let spec = d.spec.as_ref();
                row.extend(workload_wide(spec.map(|s| &s.template), spec.map(|s| &s.selector)));
            }
            row
        })
        .collect();
    Table::new(headers, rows, deployments.iter().map(|d| &d.metadata))
}

fn replicaset_table(sets: &[ReplicaSet], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "DESIRED", "CURRENT", "READY", "AGE"];
    if wide {
        headers.extend(["CONTAINERS", "IMAGES", "SELECTOR"]);
    }
    let rows: Vec<Vec<String>> = sets
        .iter()
        .map(|rs| {
            let status = rs.status.as_ref();
            let mut row = vec![
                rs.metadata.name.clone().unwrap_or_default(),
                rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1).to_string(),
                status.map_or(0, |s| s.replicas).to_string(),
                status.and_then(|s| s.ready_replicas).unwrap_or(0).to_string(),
                object_age(&rs.metadata, now),
            ];
            if wide {
                let spec = rs.spec.as_ref();
                row.extend(workload_wide(
                    spec.and_then(|s| s.template.as_ref()),
                    spec.map(|s| &s.selector),
                ));
            }
            row
        })
        .collect();
    Table::new(headers, rows, sets.iter().map(|r| &r.metadata))
}

fn configmap_table(items: &[ConfigMap], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|c| {
            let data = c.data.as_ref().map_or(0, |d| d.len()) + c.binary_data.as_ref().map_or(0, |d| d.len());
            vec![
                c.metadata.name.clone().unwrap_or_default(),
                data.to_string(),
                object_age(&c.metadata, now),
            ]
        })
        .collect();
    Table::new(vec!["NAME", "DATA", "AGE"], rows, items.iter().map(|c| &c.metadata))
}

fn job_table(items: &[Job], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "STATUS", "COMPLETIONS", "DURATION", "AGE"];
    if wide {
        headers.extend(["CONTAINERS", "IMAGES", "SELECTOR"]);
    }
    let rows = items
        .iter()
        .map(|job| {
            let status = job.status.as_ref();
            let has = |kind: &str| {
                status
                    .and_then(|s| s.conditions.as_ref())
                    .is_some_and(|c| c.iter().any(|c| c.type_ == kind && c.status == "True"))
            };
            let state = if has("Complete") {
                "Complete"
            } else if has("Failed") {
                "Failed"
            } else if job.spec.as_ref().and_then(|s| s.suspend) == Some(true) {
                "Suspended"
            } else {
                "Running"
            };
            let completions = job.spec.as_ref().and_then(|s| s.completions).unwrap_or(1);
            let started = status.and_then(|s| s.start_time.as_ref()).map(clock::secs);
            let ended = status.and_then(|s| s.completion_time.as_ref()).map(clock::secs);
            let duration = match (started, ended) {
                (Some(start), Some(end)) => human_duration(end - start),
                (Some(start), None) if state == "Running" => human_duration(clock::secs(now) - start),
                _ => "<none>".into(),
            };
            let mut row = vec![
                job.metadata.name.clone().unwrap_or_default(),
                state.to_string(),
                format!("{}/{completions}", status.and_then(|s| s.succeeded).unwrap_or(0)),
                duration,
                object_age(&job.metadata, now),
            ];
            if wide {
                let containers = job
                    .spec
                    .as_ref()
                    .and_then(|s| s.template.spec.as_ref())
                    .map(|p| p.containers.as_slice())
                    .unwrap_or_default();
                row.push(containers.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(","));
                row.push(
                    containers
                        .iter()
                        .map(|c| c.image.clone().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                let uid = job.metadata.uid.clone().unwrap_or_default();
                row.push(format!("batch.kubernetes.io/controller-uid={uid}"));
            }
            row
        })
        .collect();
    Table::new(headers, rows, items.iter().map(|j| &j.metadata))
}

fn cronjob_table(items: &[CronJob], wide: bool, now: &Time) -> Table {
    let mut headers = vec![
        "NAME",
        "SCHEDULE",
        "TIMEZONE",
        "SUSPEND",
        "ACTIVE",
        "LAST SCHEDULE",
        "AGE",
    ];
    if wide {
        headers.extend(["CONTAINERS", "IMAGES", "SELECTOR"]);
    }
    let rows = items
        .iter()
        .map(|cj| {
            let spec = Some(&cj.spec);
            let status = cj.status.as_ref();
            let mut row = vec![
                cj.metadata.name.clone().unwrap_or_default(),
                spec.map(|s| s.schedule.clone()).unwrap_or_default(),
                spec.and_then(|s| s.time_zone.clone())
                    .unwrap_or_else(|| "<none>".into()),
                spec.and_then(|s| s.suspend).unwrap_or(false).to_string(),
                status.and_then(|s| s.active.as_ref()).map_or(0, Vec::len).to_string(),
                status.and_then(|s| s.last_schedule_time.as_ref()).map_or_else(
                    || "<none>".into(),
                    |t| human_duration(clock::secs(now) - clock::secs(t)),
                ),
                object_age(&cj.metadata, now),
            ];
            if wide {
                let containers = spec
                    .and_then(|s| s.job_template.spec.as_ref())
                    .and_then(|j| j.template.spec.as_ref())
                    .map(|p| p.containers.as_slice())
                    .unwrap_or_default();
                row.push(containers.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(","));
                row.push(
                    containers
                        .iter()
                        .map(|c| c.image.clone().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                row.push("<none>".into());
            }
            row
        })
        .collect();
    Table::new(headers, rows, items.iter().map(|c| &c.metadata))
}

fn policy_table(items: &[NetworkPolicy], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|p| {
            let selector = p.spec.as_ref().and_then(|s| s.pod_selector.as_ref()).map(|s| {
                let mut parts: Vec<String> = s
                    .match_labels
                    .iter()
                    .flatten()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                parts.extend(
                    s.match_expressions
                        .iter()
                        .flatten()
                        .map(|e| format!("{} {}", e.key, e.operator)),
                );
                parts.join(",")
            });
            vec![
                p.metadata.name.clone().unwrap_or_default(),
                selector.filter(|s| !s.is_empty()).unwrap_or_else(|| "<none>".into()),
                object_age(&p.metadata, now),
            ]
        })
        .collect();
    Table::new(
        vec!["NAME", "POD-SELECTOR", "AGE"],
        rows,
        items.iter().map(|p| &p.metadata),
    )
}

fn secret_table(items: &[Secret], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|s| {
            vec![
                s.metadata.name.clone().unwrap_or_default(),
                s.type_.clone().unwrap_or_default(),
                s.data.as_ref().map_or(0, |d| d.len()).to_string(),
                object_age(&s.metadata, now),
            ]
        })
        .collect();
    Table::new(
        vec!["NAME", "TYPE", "DATA", "AGE"],
        rows,
        items.iter().map(|s| &s.metadata),
    )
}

fn namespace_table(items: &[Namespace], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|n| {
            vec![
                n.metadata.name.clone().unwrap_or_default(),
                n.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default(),
                object_age(&n.metadata, now),
            ]
        })
        .collect();
    Table::new(vec!["NAME", "STATUS", "AGE"], rows, items.iter().map(|n| &n.metadata))
}

fn or_none(text: String) -> String {
    if text.is_empty() {
        NONE.into()
    } else {
        text
    }
}

/// kubectl's node status: `Ready`, `NotReady` or `Unknown`, plus `,SchedulingDisabled`.
fn node_status(node: &Node) -> String {
    let ready = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|cs| cs.iter().find(|c| c.type_ == "Ready"));
    let mut status = match ready.map(|c| c.status.as_str()) {
        Some("True") => "Ready",
        Some("False") => "NotReady",
        _ => "Unknown",
    }
    .to_string();
    if node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true) {
        status.push_str(",SchedulingDisabled");
    }
    status
}

fn node_table(items: &[Node], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "STATUS", "ROLES", "AGE", "VERSION"];
    if wide {
        headers.extend([
            "INTERNAL-IP",
            "EXTERNAL-IP",
            "OS-IMAGE",
            "KERNEL-VERSION",
            "CONTAINER-RUNTIME",
        ]);
    }
    let rows = items
        .iter()
        .map(|n| {
            let info = n.status.as_ref().and_then(|s| s.node_info.as_ref());
            let roles = n
                .metadata
                .labels
                .iter()
                .flatten()
                .filter_map(|(k, _)| k.strip_prefix("node-role.kubernetes.io/"))
                .collect::<Vec<_>>()
                .join(",");
            let mut row = vec![
                n.metadata.name.clone().unwrap_or_default(),
                node_status(n),
                or_none(roles),
                object_age(&n.metadata, now),
                info.map(|i| i.kubelet_version.clone()).unwrap_or_default(),
            ];
            if wide {
                let address = |kind: &str| {
                    n.status
                        .as_ref()
                        .and_then(|s| s.addresses.as_ref())
                        .and_then(|a| a.iter().find(|a| a.type_ == kind))
                        .map_or_else(|| NONE.to_string(), |a| a.address.clone())
                };
                let from_info = |f: fn(&k8s_openapi::api::core::v1::NodeSystemInfo) -> &String| {
                    info.map(|i| f(i).clone())
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| "<unknown>".into())
                };
                row.extend([
                    address("InternalIP"),
                    address("ExternalIP"),
                    from_info(|i| &i.os_image),
                    from_info(|i| &i.kernel_version),
                    from_info(|i| &i.container_runtime_version),
                ]);
            }
            row
        })
        .collect();
    Table::new(headers, rows, items.iter().map(|n| &n.metadata))
}

/// `10.244.0.5:8080,10.244.1.7:8080 + 1 more...`: kubectl shows at most three endpoints.
fn endpoint_list(endpoints: &[String]) -> String {
    match endpoints.len() {
        0 => "<none>".into(),
        1..=3 => endpoints.join(","),
        n => format!("{} + {} more...", endpoints[..3].join(","), n - 3),
    }
}

fn endpoints_table(items: &[Endpoints], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|e| {
            let mut addresses = Vec::new();
            for subset in e.subsets.iter().flatten() {
                for address in subset.addresses.iter().flatten() {
                    for port in subset.ports.iter().flatten() {
                        addresses.push(format!("{}:{}", address.ip, port.port));
                    }
                    if subset.ports.as_ref().is_none_or(Vec::is_empty) {
                        addresses.push(address.ip.clone());
                    }
                }
            }
            vec![
                e.metadata.name.clone().unwrap_or_default(),
                endpoint_list(&addresses),
                object_age(&e.metadata, now),
            ]
        })
        .collect();
    Table::new(
        vec!["NAME", "ENDPOINTS", "AGE"],
        rows,
        items.iter().map(|e| &e.metadata),
    )
}

fn slice_table(items: &[EndpointSlice], now: &Time) -> Table {
    let rows = items
        .iter()
        .map(|s| {
            let ports = s
                .ports
                .iter()
                .flatten()
                .map(|p| p.port.map_or_else(|| "*".to_string(), |p| p.to_string()))
                .collect::<Vec<_>>()
                .join(",");
            let addresses: Vec<String> = s
                .endpoints
                .iter()
                .flatten()
                .flat_map(|e| e.addresses.iter().cloned())
                .collect();
            vec![
                s.metadata.name.clone().unwrap_or_default(),
                s.address_type.clone(),
                or_none(ports),
                endpoint_list(&addresses),
                object_age(&s.metadata, now),
            ]
        })
        .collect();
    Table::new(
        vec!["NAME", "ADDRESSTYPE", "PORTS", "ENDPOINTS", "AGE"],
        rows,
        items.iter().map(|s| &s.metadata),
    )
}

fn service_ports(service: &Service) -> String {
    let ports = service.spec.as_ref().and_then(|s| s.ports.as_ref());
    let text = ports
        .into_iter()
        .flatten()
        .map(|p| {
            let protocol = p.protocol.as_deref().unwrap_or("TCP");
            match p.node_port {
                Some(node_port) if node_port != 0 => format!("{}:{node_port}/{protocol}", p.port),
                _ => format!("{}/{protocol}", p.port),
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    or_none(text)
}

fn service_external_ip(service: &Service) -> String {
    let Some(spec) = service.spec.as_ref() else {
        return NONE.into();
    };
    match spec.type_.as_deref() {
        Some("ExternalName") => spec.external_name.clone().unwrap_or_else(|| NONE.into()),
        Some("LoadBalancer") => {
            let ingress = service
                .status
                .as_ref()
                .and_then(|s| s.load_balancer.as_ref())
                .and_then(|l| l.ingress.as_ref());
            let ips = ingress
                .into_iter()
                .flatten()
                .filter_map(|i| i.ip.clone().or_else(|| i.hostname.clone()))
                .collect::<Vec<_>>()
                .join(",");
            if ips.is_empty() {
                "<pending>".into()
            } else {
                ips
            }
        }
        _ => or_none(
            spec.external_ips
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        ),
    }
}

fn service_table(items: &[Service], wide: bool, now: &Time) -> Table {
    let mut headers = vec!["NAME", "TYPE", "CLUSTER-IP", "EXTERNAL-IP", "PORT(S)", "AGE"];
    if wide {
        headers.push("SELECTOR");
    }
    let rows = items
        .iter()
        .map(|s| {
            let spec = s.spec.as_ref();
            let mut row = vec![
                s.metadata.name.clone().unwrap_or_default(),
                spec.and_then(|p| p.type_.clone()).unwrap_or_default(),
                spec.and_then(|p| p.cluster_ip.clone())
                    .filter(|ip| !ip.is_empty())
                    .unwrap_or_else(|| NONE.into()),
                service_external_ip(s),
                service_ports(s),
                object_age(&s.metadata, now),
            ];
            if wide {
                let selector = spec
                    .and_then(|p| p.selector.as_ref())
                    .map(|m| m.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                row.push(or_none(selector));
            }
            row
        })
        .collect();
    Table::new(headers, rows, items.iter().map(|s| &s.metadata))
}

fn pod_ready(pod: &Pod) -> String {
    let total = pod.spec.as_ref().map_or(0, |s| s.containers.len());
    let ready = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .map_or(0, |cs| cs.iter().filter(|c| c.ready).count());
    format!("{ready}/{total}")
}

fn pod_restarts(pod: &Pod) -> i32 {
    pod.status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .map_or(0, |cs| cs.iter().map(|c| c.restart_count).sum())
}

/// Mirrors the reason selection in kubectl's pod printer.
fn pod_status(pod: &Pod) -> String {
    let status = pod.status.as_ref();
    let mut reason = status
        .and_then(|s| s.reason.clone().or_else(|| s.phase.clone()))
        .unwrap_or_default();

    let init_statuses = status
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or_default();
    let init_total = pod
        .spec
        .as_ref()
        .and_then(|s| s.init_containers.as_ref())
        .map_or(0, Vec::len);
    let mut initializing = false;
    for (i, init) in init_statuses.iter().enumerate() {
        let state = init.state.as_ref();
        if let Some(t) = state.and_then(|s| s.terminated.as_ref()) {
            if t.exit_code == 0 {
                continue;
            }
            initializing = true;
            reason = match (&t.reason, t.signal) {
                (Some(r), _) if !r.is_empty() => format!("Init:{r}"),
                (_, Some(sig)) if sig != 0 => format!("Init:Signal:{sig}"),
                _ => format!("Init:ExitCode:{}", t.exit_code),
            };
        } else if let Some(w) = state.and_then(|s| s.waiting.as_ref()).filter(|w| {
            w.reason
                .as_deref()
                .is_some_and(|r| !r.is_empty() && r != "PodInitializing")
        }) {
            initializing = true;
            reason = format!("Init:{}", w.reason.as_deref().unwrap_or_default());
        } else {
            initializing = true;
            reason = format!("Init:{i}/{init_total}");
        }
        break;
    }

    if !initializing {
        let containers = status.and_then(|s| s.container_statuses.as_deref()).unwrap_or_default();
        for c in containers.iter().rev() {
            let state = c.state.as_ref();
            if let Some(r) = state
                .and_then(|s| s.waiting.as_ref())
                .and_then(|w| w.reason.as_ref())
                .filter(|r| !r.is_empty())
            {
                reason = r.clone();
            } else if let Some(t) = state.and_then(|s| s.terminated.as_ref()) {
                reason = match (&t.reason, t.signal) {
                    (Some(r), _) if !r.is_empty() => r.clone(),
                    (_, Some(sig)) if sig != 0 => format!("Signal:{sig}"),
                    _ => format!("ExitCode:{}", t.exit_code),
                };
            }
        }
    }

    if pod.metadata.deletion_timestamp.is_some() {
        reason = if status.and_then(|s| s.reason.as_deref()) == Some("NodeLost") {
            "Unknown".into()
        } else {
            "Terminating".into()
        };
    }
    reason
}

fn pod_age(pod: &Pod, now: &Time) -> String {
    match &pod.metadata.creation_timestamp {
        Some(created) => human_duration(now.0.as_second() - created.0.as_second()),
        None => "<unknown>".into(),
    }
}

/// kubectl's `duration.HumanDuration`, from whole seconds.
pub(crate) fn human_duration(seconds: i64) -> String {
    if seconds < -1 {
        return "<invalid>".into();
    }
    if seconds < 0 {
        return "0s".into();
    }
    if seconds < 120 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 10 {
        return match seconds % 60 {
            0 => format!("{minutes}m"),
            s => format!("{minutes}m{s}s"),
        };
    }
    if minutes < 180 {
        return format!("{minutes}m");
    }
    let hours = seconds / 3600;
    if hours < 8 {
        return match minutes % 60 {
            0 => format!("{hours}h"),
            m => format!("{hours}h{m}m"),
        };
    }
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if hours < 24 * 8 {
        return match hours % 24 {
            0 => format!("{days}d"),
            h => format!("{days}d{h}h"),
        };
    }
    if hours < 24 * 365 * 2 {
        return format!("{days}d");
    }
    let years = days / 365;
    if hours < 24 * 365 * 8 {
        return match days % 365 {
            0 => format!("{years}y"),
            d => format!("{years}y{d}d"),
        };
    }
    format!("{years}y")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> Time {
        serde_json::from_value(json!("2026-10-27T18:30:00Z")).unwrap()
    }

    fn render_kind(kind: &str, items: &[Value], format: OutputFormat, single: bool, now: &Time) -> String {
        let options = RenderOptions {
            format: &format,
            single,
            all_namespaces: false,
            no_headers: false,
            show_labels: false,
            now,
        };
        render(crate::api::info(kind).unwrap(), items, &options).unwrap()
    }

    fn render_pods(pods: &[Pod], format: OutputFormat, single: bool, now: &Time) -> String {
        let items: Vec<Value> = pods.iter().map(|p| serde_json::to_value(p).unwrap()).collect();
        render_kind("Pod", &items, format, single, now)
    }

    fn event(reason: &str, count: i32, first: &str, last: &str) -> Value {
        json!({
            "apiVersion": "v1", "kind": "Event",
            "metadata": {"name": format!("p.{reason}"), "namespace": "default"},
            "involvedObject": {"kind": "Pod", "name": "p", "fieldPath": "spec.containers{app}"},
            "reason": reason, "message": format!("{reason} message"), "type": "Warning",
            "source": {"component": "kubelet", "host": "worker-1"},
            "firstTimestamp": first, "lastTimestamp": last, "count": count
        })
    }

    fn pod(value: serde_json::Value) -> Pod {
        let mut base =
            json!({"apiVersion": "v1", "kind": "Pod", "metadata": {}, "spec": {"containers": [{"name": "app"}]}});
        base["metadata"] = value["metadata"].clone();
        if let Some(spec) = value.get("spec") {
            base["spec"] = spec.clone();
        }
        if let Some(status) = value.get("status") {
            base["status"] = status.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn container_status(ready: bool, restarts: i32, state: serde_json::Value) -> serde_json::Value {
        json!({"name": "app", "ready": ready, "restartCount": restarts, "image": "img", "imageID": "", "state": state})
    }

    fn crashloop_pod() -> Pod {
        pod(json!({
            "metadata": {"name": "payments", "creationTimestamp": "2026-10-27T17:43:00Z"},
            "status": {"phase": "Running", "containerStatuses": [
                container_status(false, 14, json!({"waiting": {"reason": "CrashLoopBackOff"}}))
            ]}
        }))
    }

    #[test]
    fn human_duration_matches_kubectl() {
        let cases = [
            (-5, "<invalid>"),
            (-1, "0s"),
            (0, "0s"),
            (45, "45s"),
            (119, "119s"),
            (120, "2m"),
            (125, "2m5s"),
            (599, "9m59s"),
            (600, "10m"),
            (3 * 3600 - 1, "179m"),
            (3 * 3600, "3h"),
            (3 * 3600 + 60, "3h1m"),
            (8 * 3600, "8h"),
            (47 * 3600, "47h"),
            (48 * 3600, "2d"),
            (52 * 3600, "2d4h"),
            (8 * 86400, "8d"),
            (400 * 86400, "400d"),
            (800 * 86400, "2y70d"),
            (3000 * 86400, "8y"),
        ];
        for (secs, want) in cases {
            assert_eq!(human_duration(secs), want, "{secs}s");
        }
    }

    #[test]
    fn crashloop_columns() {
        let p = crashloop_pod();
        assert_eq!(pod_ready(&p), "0/1");
        assert_eq!(pod_status(&p), "CrashLoopBackOff");
        assert_eq!(pod_restarts(&p), 14);
        assert_eq!(pod_age(&p, &now()), "47m");
    }

    #[test]
    fn running_multi_container_ready_count() {
        let p = pod(json!({
            "metadata": {"name": "m"},
            "spec": {"containers": [{"name": "a"}, {"name": "b"}]},
            "status": {"phase": "Running", "containerStatuses": [
                container_status(true, 1, json!({"running": {}})),
                container_status(false, 2, json!({"running": {}})),
            ]}
        }));
        assert_eq!(pod_ready(&p), "1/2");
        assert_eq!(pod_restarts(&p), 3);
        assert_eq!(pod_status(&p), "Running");
    }

    #[test]
    fn terminated_and_terminating_status() {
        let oom = pod(
            json!({"metadata": {"name": "a"}, "status": {"phase": "Running", "containerStatuses": [
            container_status(false, 0, json!({"terminated": {"exitCode": 137, "reason": "OOMKilled"}}))]}}),
        );
        assert_eq!(pod_status(&oom), "OOMKilled");

        let exit = pod(
            json!({"metadata": {"name": "a"}, "status": {"phase": "Running", "containerStatuses": [
            container_status(false, 0, json!({"terminated": {"exitCode": 2}}))]}}),
        );
        assert_eq!(pod_status(&exit), "ExitCode:2");

        let deleting = pod(
            json!({"metadata": {"name": "a", "deletionTimestamp": "2026-10-27T18:00:00Z"}, "status": {"phase": "Running"}}),
        );
        assert_eq!(pod_status(&deleting), "Terminating");
    }

    #[test]
    fn init_container_status() {
        let init = |state: serde_json::Value| {
            pod(json!({"metadata": {"name": "a"},
                "spec": {"containers": [{"name": "app"}], "initContainers": [{"name": "setup"}]},
                "status": {"phase": "Pending", "initContainerStatuses": [container_status(false, 0, state)]}}))
        };
        assert_eq!(pod_status(&init(json!({"running": {}}))), "Init:0/1");
        assert_eq!(
            pod_status(&init(json!({"waiting": {"reason": "ImagePullBackOff"}}))),
            "Init:ImagePullBackOff"
        );
        assert_eq!(
            pod_status(&init(json!({"terminated": {"exitCode": 1}}))),
            "Init:ExitCode:1"
        );
    }

    #[test]
    fn missing_creation_timestamp_is_unknown_age() {
        assert_eq!(pod_age(&pod(json!({"metadata": {"name": "a"}})), &now()), "<unknown>");
    }

    #[test]
    fn table_is_aligned_and_wide_adds_columns() {
        let pods = [
            crashloop_pod(),
            pod(json!({"metadata": {"name": "a"}, "status": {"phase": "Pending"}})),
        ];
        let table = render_pods(&pods, OutputFormat::Table, false, &now());
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "NAME       READY   STATUS             RESTARTS   AGE");
        assert_eq!(lines[1], "payments   0/1     CrashLoopBackOff   14         47m");
        assert_eq!(lines[2], "a          0/1     Pending            0          <unknown>");

        let wide = render_pods(&pods, OutputFormat::Wide, false, &now());
        let columns = |line: &str| -> Vec<String> {
            line.split("   ")
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect()
        };
        let mut wide_lines = wide.lines();
        assert_eq!(
            columns(wide_lines.next().unwrap())[5..],
            ["IP", "NODE", "NOMINATED NODE", "READINESS GATES"]
        );
        assert_eq!(
            columns(wide_lines.next().unwrap())[5..],
            ["<none>", "<none>", "<none>", "<none>"]
        );
    }

    #[test]
    fn name_format() {
        let pods = [crashloop_pod()];
        assert_eq!(render_pods(&pods, OutputFormat::Name, false, &now()), "pod/payments");
    }

    #[test]
    fn json_single_is_bare_object_and_multiple_is_list() {
        let pods = [crashloop_pod()];
        let single = render_pods(&pods, OutputFormat::Json, true, &now());
        let value: serde_json::Value = serde_json::from_str(&single).unwrap();
        assert_eq!(value["kind"], "Pod");
        assert!(single.contains("\n    \"kind\""), "four-space indent: {single}");

        let list = render_pods(&pods, OutputFormat::Json, false, &now());
        let value: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert_eq!(value["kind"], "List");
        assert_eq!(value["items"].as_array().unwrap().len(), 1);
        assert_eq!(value["metadata"]["resourceVersion"], "");
    }

    #[test]
    fn empty_json_list_is_still_a_list() {
        let list = render_pods(&[], OutputFormat::Json, false, &now());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&list).unwrap()["items"],
            json!([])
        );
    }

    #[test]
    fn yaml_format() {
        let out = render_pods(&[crashloop_pod()], OutputFormat::Yaml, true, &now());
        assert!(out.starts_with("apiVersion: v1\nkind: Pod\nmetadata:"), "{out}");
        assert!(out.contains("name: payments"));
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn events_table_shows_repeat_counts_sorted_by_last_seen() {
        let items = [
            event("BackOff", 14, "2026-10-27T17:44:00Z", "2026-10-27T18:27:00Z"),
            event("Scheduled", 1, "2026-10-27T17:43:00Z", "2026-10-27T17:43:00Z"),
        ];
        let out = render_kind("Event", &items, OutputFormat::Table, false, &now());
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("LAST SEEN"), "{out}");
        assert!(lines[1].contains("Scheduled") && lines[1].starts_with("47m "), "{out}");
        assert!(
            lines[2].contains("BackOff") && lines[2].starts_with("3m (x14 over 46m) "),
            "{out}"
        );
        assert!(lines[2].contains("pod/p") && lines[2].ends_with("BackOff message"));
    }

    #[test]
    fn events_wide_adds_source_and_counts() {
        let items = [event("BackOff", 2, "2026-10-27T18:00:00Z", "2026-10-27T18:10:00Z")];
        let out = render_kind("Event", &items, OutputFormat::Wide, false, &now());
        assert!(out.lines().next().unwrap().contains("SUBOBJECT"));
        let row = out.lines().nth(1).unwrap();
        assert!(
            row.contains("spec.containers{app}") && row.contains("kubelet, worker-1") && row.contains("p.BackOff"),
            "{row}"
        );
    }

    #[test]
    fn name_format_uses_kind_prefix() {
        let items = [event("BackOff", 1, "2026-10-27T18:00:00Z", "2026-10-27T18:00:00Z")];
        assert_eq!(
            render_kind("Event", &items, OutputFormat::Name, false, &now()),
            "event/p.BackOff"
        );
    }
}
