//! Success criteria: did the player solve the case?
//!
//! A scenario declares, as open data:
//! - `success.assert`: facts about the cluster that must all hold, continuously, for
//!   `stableSeconds` of cluster time (so a fix that flaps does not count);
//! - `success.constraints`: things that must never be broken on the way (checked at every
//!   observed instant, sticky once broken) and commands that must never be run (`forbid`);
//! - `findings`: hidden clues unlocked by what the player investigates.
//!
//! Everything is evaluated by the engine from state and parsed commands, so the same transcript
//! always yields the same verdict. That is what makes stateless score verification possible.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api;
use crate::parser::{Command, ResourceKind};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::{ClusterStore, StoreError};

const VERBS: [&str; 8] = [
    "get",
    "describe",
    "logs",
    "set-env",
    "rollout-restart",
    "delete",
    "apply",
    "create",
];
const DEFAULT_STABLE_SECS: i64 = 15;

// ---- scenario data --------------------------------------------------------------------------

/// Which objects a rule is about.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Select {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: Option<String>,
    /// Every listed label must match.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// How many selected objects there may be.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Count {
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub exactly: Option<usize>,
}

/// A condition on one value inside each selected manifest. All given operators must hold.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Cond {
    pub path: String,
    pub equals: Option<Value>,
    pub not_equals: Option<Value>,
    pub exists: Option<bool>,
    pub at_least: Option<f64>,
    pub at_most: Option<f64>,
    pub contains: Option<Value>,
}

/// Matches a parsed kubectl command (never raw text).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMatcher {
    /// `get`, `describe`, `logs`, `set-env`, `rollout-restart`, `delete`, `apply` or `create`.
    pub verb: String,
    /// Resource kind as kubectl names it (`pods`, `deploy`, ...).
    pub resource: Option<String>,
    /// Object name; a trailing `*` makes it a prefix.
    pub name: Option<String>,
    /// For `set-env`: whether `--list` was given.
    pub list: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub label: String,
    pub select: Option<Select>,
    pub count: Option<Count>,
    #[serde(default)]
    pub each: Vec<Cond>,
    /// Constraints only: a command that must never be run.
    pub forbid: Option<CommandMatcher>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Success {
    #[serde(default = "default_stable")]
    pub stable_seconds: i64,
    #[serde(rename = "assert")]
    pub assertions: Vec<Rule>,
    #[serde(default)]
    pub constraints: Vec<Rule>,
}

fn default_stable() -> i64 {
    DEFAULT_STABLE_SECS
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub command: Option<CommandMatcher>,
    #[serde(rename = "anyOf")]
    pub any_of: Option<Vec<When>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub clue: String,
    pub when: When,
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub success: Option<Success>,
    pub findings: Vec<Finding>,
}

// ---- validation -----------------------------------------------------------------------------

impl Rules {
    /// Check the rules for mistakes an author could make, with errors that name the rule.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(success) = &self.success {
            if success.stable_seconds < 0 {
                return Err("success.stableSeconds cannot be negative".into());
            }
            if success.assertions.is_empty() {
                return Err("success.assert must list at least one assertion".into());
            }
            for (i, rule) in success.assertions.iter().enumerate() {
                let at = format!("success.assert[{i}] ({:?})", rule.label);
                if rule.forbid.is_some() {
                    return Err(format!("{at}: `forbid` is only allowed in constraints"));
                }
                if rule.select.is_none() {
                    return Err(format!("{at}: needs a `select`"));
                }
                validate_rule(rule).map_err(|e| format!("{at}: {e}"))?;
            }
            for (i, rule) in success.constraints.iter().enumerate() {
                let at = format!("success.constraints[{i}] ({:?})", rule.label);
                if rule.select.is_some() == rule.forbid.is_some() {
                    return Err(format!("{at}: needs exactly one of `select` or `forbid`"));
                }
                validate_rule(rule).map_err(|e| format!("{at}: {e}"))?;
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for (i, finding) in self.findings.iter().enumerate() {
            let at = format!("findings[{i}] ({:?})", finding.id);
            if finding.id.is_empty() || !seen.insert(finding.id.as_str()) {
                return Err(format!("{at}: ids must be non-empty and unique"));
            }
            if finding.clue.trim().is_empty() {
                return Err(format!("{at}: needs a `clue`"));
            }
            validate_when(&finding.when).map_err(|e| format!("{at}: {e}"))?;
        }
        Ok(())
    }
}

fn validate_rule(rule: &Rule) -> Result<(), String> {
    if rule.label.trim().is_empty() {
        return Err("needs a `label`".into());
    }
    if let Some(select) = &rule.select {
        if !api::kinds().iter().any(|k| k.kind == select.kind) {
            let known: Vec<&str> = api::kinds().iter().map(|k| k.kind).collect();
            return Err(format!(
                "unknown kind {:?} (expected {})",
                select.kind,
                known.join(", ")
            ));
        }
    }
    for cond in &rule.each {
        parse_path(&cond.path).map_err(|e| format!("path {:?}: {e}", cond.path))?;
        let ops = [
            cond.equals.is_some(),
            cond.not_equals.is_some(),
            cond.exists.is_some(),
            cond.at_least.is_some(),
            cond.at_most.is_some(),
            cond.contains.is_some(),
        ];
        if !ops.contains(&true) {
            return Err(format!(
                "condition on {:?} needs one of equals, notEquals, exists, atLeast, atMost, contains",
                cond.path
            ));
        }
    }
    if let Some(forbid) = &rule.forbid {
        validate_matcher(forbid)?;
    }
    Ok(())
}

fn validate_matcher(matcher: &CommandMatcher) -> Result<(), String> {
    if !VERBS.contains(&matcher.verb.as_str()) {
        return Err(format!(
            "unknown verb {:?} (expected {})",
            matcher.verb,
            VERBS.join(", ")
        ));
    }
    if let Some(resource) = &matcher.resource {
        api::kind_for_arg(resource).ok_or_else(|| format!("unknown resource {resource:?}"))?;
    }
    Ok(())
}

fn validate_when(when: &When) -> Result<(), String> {
    match (&when.command, &when.any_of) {
        (Some(command), None) => validate_matcher(command),
        (None, Some(any)) if !any.is_empty() => any.iter().try_for_each(validate_when),
        _ => Err("`when` needs exactly one of `command` or a non-empty `anyOf`".into()),
    }
}

// ---- paths and conditions -------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Segment {
    Field(String),
    Index(usize),
    /// `[key=value]`: the array element whose `key` is `value`.
    Filter(String, String),
}

/// `status.conditions[type=Ready].status`, `spec.containers[0].image`, `metadata.labels["a.b/c"]`.
fn parse_path(path: &str) -> Result<Vec<Segment>, String> {
    let chars: Vec<char> = path.chars().collect();
    let mut segments = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' => i += 1,
            '[' => {
                let close = chars[i..].iter().position(|c| *c == ']').ok_or("unclosed `[`")? + i;
                let inner: String = chars[i + 1..close].iter().collect();
                segments.push(bracket(&inner)?);
                i = close + 1;
            }
            _ => {
                let start = i;
                while i < chars.len() && chars[i] != '.' && chars[i] != '[' {
                    i += 1;
                }
                segments.push(Segment::Field(chars[start..i].iter().collect()));
            }
        }
    }
    if segments.is_empty() {
        return Err("is empty".into());
    }
    Ok(segments)
}

fn bracket(inner: &str) -> Result<Segment, String> {
    if let Some(quoted) = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Ok(Segment::Field(quoted.to_string()));
    }
    if let Ok(index) = inner.parse::<usize>() {
        return Ok(Segment::Index(index));
    }
    match inner.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok(Segment::Filter(key.to_string(), value.to_string())),
        _ => Err(format!(
            "cannot read `[{inner}]` (use an index, key=value, or a quoted name)"
        )),
    }
}

fn lookup<'a>(value: &'a Value, segments: &[Segment]) -> Option<&'a Value> {
    let mut current = value;
    for segment in segments {
        current = match segment {
            Segment::Field(name) => current.get(name)?,
            Segment::Index(i) => current.get(*i)?,
            Segment::Filter(key, wanted) => current.as_array()?.iter().find(|e| match e.get(key) {
                Some(Value::String(s)) => s == wanted,
                Some(other) => {
                    let text = other.to_string();
                    text == *wanted
                }
                None => false,
            })?,
        };
    }
    Some(current)
}

/// JSON equality where `1` and `1.0` are the same number.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn cond_holds(cond: &Cond, manifest: &Value) -> bool {
    let Ok(path) = parse_path(&cond.path) else { return false };
    let found = lookup(manifest, &path).filter(|v| !v.is_null());

    if let Some(want) = cond.exists {
        if found.is_some() != want {
            return false;
        }
    }
    if let Some(expected) = &cond.equals {
        if !found.is_some_and(|v| same(v, expected)) {
            return false;
        }
    }
    if let Some(unexpected) = &cond.not_equals {
        if found.is_some_and(|v| same(v, unexpected)) {
            return false;
        }
    }
    if let Some(min) = cond.at_least {
        if !found.and_then(Value::as_f64).is_some_and(|n| n >= min) {
            return false;
        }
    }
    if let Some(max) = cond.at_most {
        if !found.and_then(Value::as_f64).is_some_and(|n| n <= max) {
            return false;
        }
    }
    if let Some(needle) = &cond.contains {
        let ok = match found {
            Some(Value::String(s)) => needle.as_str().is_some_and(|n| s.contains(n)),
            Some(Value::Array(items)) => items.iter().any(|i| same(i, needle)),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

// ---- evaluating rules against the cluster ---------------------------------------------------

fn select(store: &ClusterStore, select: &Select) -> Result<Vec<Value>, StoreError> {
    // Cluster-scoped kinds (Node, Namespace) are stored outside any namespace.
    let namespaced = api::kinds()
        .iter()
        .find(|k| k.kind == select.kind)
        .is_none_or(|k| k.namespaced);
    let namespace = match namespaced {
        true => select.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE),
        false => "",
    };
    let candidates = match &select.name {
        Some(name) => store.get(&select.kind, namespace, name)?.into_iter().collect(),
        None => store.list(&select.kind, namespace)?,
    };
    Ok(candidates
        .into_iter()
        .filter(|m| {
            select
                .labels
                .iter()
                .all(|(k, v)| m["metadata"]["labels"][k].as_str() == Some(v.as_str()))
        })
        .collect())
}

fn count_ok(count: &Option<Count>, n: usize, default_min: usize) -> bool {
    match count {
        None => n >= default_min,
        Some(c) => match c.exactly {
            Some(exactly) => n == exactly,
            None => n >= c.min.unwrap_or(0) && c.max.is_none_or(|max| n <= max),
        },
    }
}

/// Does a state rule hold right now? (`forbid` rules are about commands, not state.)
fn holds(rule: &Rule, store: &ClusterStore, default_min: usize) -> Result<bool, StoreError> {
    let Some(sel) = &rule.select else { return Ok(true) };
    let objects = select(store, sel)?;
    Ok(count_ok(&rule.count, objects.len(), default_min)
        && objects.iter().all(|m| rule.each.iter().all(|c| cond_holds(c, m))))
}

// ---- command matching -----------------------------------------------------------------------

fn glob(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// An object a command wrote or deleted: `(kind, name)`.
pub type Touched = (&'static str, String);

/// Commands that act on the objects in files; they are judged by what they actually touched.
fn is_file_command(command: &Command) -> bool {
    matches!(
        command,
        Command::Apply(_) | Command::Create(_) | Command::DeleteFiles(_)
    )
}

fn matches_command(matcher: &CommandMatcher, command: &Command, touched: &[Touched]) -> bool {
    let kind = matcher.resource.as_deref().and_then(api::kind_for_arg).map(|k| k.kind);
    let wanted = |resource: ResourceKind| kind.is_none_or(|k| k == resource.kind());
    let named = |names: &[&str]| match &matcher.name {
        None => true,
        Some(pattern) => names.iter().any(|n| glob(pattern, n)),
    };
    let touched_one = || {
        touched.iter().any(|(k, name)| {
            kind.is_none_or(|want| want == *k) && matcher.name.as_deref().is_none_or(|p| glob(p, name))
        })
    };

    match (matcher.verb.as_str(), command) {
        ("get", Command::Get(a)) => {
            wanted(a.resource) && named(&a.names.iter().map(String::as_str).collect::<Vec<_>>())
        }
        ("describe", Command::Describe(a)) => {
            wanted(a.resource) && named(&a.names.iter().map(String::as_str).collect::<Vec<_>>())
        }
        ("logs", Command::Logs(a)) => wanted(a.resource) && named(&[a.name.as_str()]),
        ("set-env", Command::SetEnv(a)) => {
            wanted(a.resource) && named(&[a.name.as_str()]) && matcher.list.is_none_or(|l| l == a.list)
        }
        ("rollout-restart", Command::RolloutRestart(a)) => {
            wanted(a.resource) && named(&a.names.iter().map(String::as_str).collect::<Vec<_>>())
        }
        ("delete", Command::Delete(a)) => {
            wanted(a.resource) && named(&a.names.iter().map(String::as_str).collect::<Vec<_>>())
        }
        ("delete", Command::DeleteFiles(_)) | ("apply", Command::Apply(_)) | ("create", Command::Create(_)) => {
            touched_one()
        }
        _ => false,
    }
}

fn when_matches(when: &When, command: &Command, touched: &[Touched]) -> bool {
    match (&when.command, &when.any_of) {
        (Some(matcher), _) => matches_command(matcher, command, touched),
        (None, Some(any)) => any.iter().any(|w| when_matches(w, command, touched)),
        (None, None) => false,
    }
}

/// kubectl reports failures as text; a command that failed does not count as an investigation.
fn is_error_output(output: &str) -> bool {
    output.starts_with("error:") || output.starts_with("Error from server") || output.starts_with("The Pod ")
}

// ---- the judge ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Solved without breaking any constraint.
    Closed,
    /// Solved, but something that should have been left alone was not.
    ClosedWithCost,
}

#[derive(Debug, Clone)]
pub struct Judge {
    rules: Rules,
    /// Cluster time the player started at; `None` while the scenario is still warming up.
    started_at: Option<i64>,
    /// Since when every assertion has held without interruption.
    holding_since: Option<i64>,
    /// Constraint index -> when it was first broken.
    broken: BTreeMap<usize, i64>,
    /// Findings in discovery order: (index into `rules.findings`, time).
    found: Vec<(usize, i64)>,
    commands: u32,
    solved: Option<(Outcome, i64)>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundFinding {
    pub id: String,
    pub clue: String,
    pub at_seconds: i64,
}

#[derive(Debug, Serialize)]
pub struct FindingsStatus {
    pub total: usize,
    pub found: Vec<FoundFinding>,
}

#[derive(Debug, Serialize)]
pub struct AssertionReport {
    pub label: String,
    pub ok: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintReport {
    pub label: String,
    pub ok: bool,
    pub broken_at_seconds: Option<i64>,
}

/// Shown only once the case is closed, so hidden rule labels do not leak while playing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub assertions: Vec<AssertionReport>,
    pub constraints: Vec<ConstraintReport>,
    /// Clues the player never uncovered, revealed now that nothing can be spoiled.
    pub missed_findings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub solved: bool,
    /// `closed` or `closedWithCost` once solved.
    pub outcome: Option<&'static str>,
    pub solved_at_seconds: Option<i64>,
    pub elapsed_seconds: i64,
    pub commands: u32,
    pub findings: FindingsStatus,
    pub report: Option<Report>,
}

impl Judge {
    pub fn new(rules: Rules) -> Self {
        Judge {
            rules,
            started_at: None,
            holding_since: None,
            broken: BTreeMap::new(),
            found: Vec::new(),
            commands: 0,
            solved: None,
        }
    }

    /// Begin judging at `now`; nothing before this (the scenario's backstory) counts.
    pub fn start(&mut self, now: i64) {
        self.started_at = Some(now);
    }

    fn constraints(&self) -> &[Rule] {
        self.rules.success.as_ref().map_or(&[], |s| s.constraints.as_slice())
    }

    /// Look at the cluster at `now`. Call after every change (each reconcile, each command).
    pub fn observe(&mut self, store: &ClusterStore, now: i64) {
        if self.started_at.is_none() || self.solved.is_some() {
            return;
        }
        let Some(success) = self.rules.success.as_ref() else {
            return;
        };

        for (i, rule) in success.constraints.iter().enumerate() {
            if rule.select.is_some() && !self.broken.contains_key(&i) && !holds(rule, store, 0).unwrap_or(true) {
                self.broken.insert(i, now);
            }
        }

        let all_hold = success
            .assertions
            .iter()
            .all(|rule| holds(rule, store, 1).unwrap_or(false));
        if !all_hold {
            self.holding_since = None;
            return;
        }
        let since = *self.holding_since.get_or_insert(now);
        if now - since >= success.stable_seconds {
            let outcome = if self.broken.is_empty() {
                Outcome::Closed
            } else {
                Outcome::ClosedWithCost
            };
            self.solved = Some((outcome, since + success.stable_seconds));
        }
    }

    /// When the judge next needs the clock to stop: the moment the stability window completes.
    pub fn next_wake(&self) -> Option<i64> {
        let success = self.rules.success.as_ref()?;
        match (self.started_at, self.solved, self.holding_since) {
            (Some(_), None, Some(since)) => Some(since + success.stable_seconds),
            _ => None,
        }
    }

    /// Note a command the player ran (it parsed), the objects it wrote or deleted (for commands
    /// that act on files) and what it printed.
    pub fn record_command(&mut self, command: &Command, touched: &[Touched], output: &str, now: i64) {
        if self.started_at.is_none() || self.solved.is_some() {
            return;
        }
        self.commands += 1;
        // A file command that failed for some documents still did what it did to the others.
        if is_file_command(command) {
            if touched.is_empty() {
                return;
            }
        } else if is_error_output(output) {
            return;
        }

        for (i, finding) in self.rules.findings.iter().enumerate() {
            if !self.found.iter().any(|(f, _)| *f == i) && when_matches(&finding.when, command, touched) {
                self.found.push((i, now));
            }
        }
        let forbidden: Vec<usize> = self
            .constraints()
            .iter()
            .enumerate()
            .filter(|(i, rule)| {
                !self.broken.contains_key(i)
                    && rule
                        .forbid
                        .as_ref()
                        .is_some_and(|m| matches_command(m, command, touched))
            })
            .map(|(i, _)| i)
            .collect();
        for i in forbidden {
            self.broken.insert(i, now);
        }
    }

    pub fn status(&self, now: i64) -> Status {
        let started = self.started_at.unwrap_or(now);
        let solved = self.solved;
        let end = solved.map_or(now, |(_, at)| at);

        let report = solved.map(|_| Report {
            assertions: self
                .rules
                .success
                .iter()
                .flat_map(|s| s.assertions.iter())
                .map(|r| AssertionReport {
                    label: r.label.clone(),
                    ok: true,
                })
                .collect(),
            constraints: self
                .constraints()
                .iter()
                .enumerate()
                .map(|(i, r)| ConstraintReport {
                    label: r.label.clone(),
                    ok: !self.broken.contains_key(&i),
                    broken_at_seconds: self.broken.get(&i).map(|at| at - started),
                })
                .collect(),
            missed_findings: self
                .rules
                .findings
                .iter()
                .enumerate()
                .filter(|(i, _)| !self.found.iter().any(|(f, _)| f == i))
                .map(|(_, f)| f.clue.clone())
                .collect(),
        });

        Status {
            solved: solved.is_some(),
            outcome: solved.map(|(o, _)| match o {
                Outcome::Closed => "closed",
                Outcome::ClosedWithCost => "closedWithCost",
            }),
            solved_at_seconds: solved.map(|(_, at)| at - started),
            elapsed_seconds: end - started,
            commands: self.commands,
            findings: FindingsStatus {
                total: self.rules.findings.len(),
                found: self
                    .found
                    .iter()
                    .map(|(i, at)| FoundFinding {
                        id: self.rules.findings[*i].id.clone(),
                        clue: self.rules.findings[*i].clue.clone(),
                        at_seconds: at - started,
                    })
                    .collect(),
            },
            report,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::parser::parse;

    fn path(p: &str) -> Vec<Segment> {
        parse_path(p).unwrap()
    }

    fn cond(value: Value) -> Cond {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parses_paths() {
        assert_eq!(
            path("status.conditions[type=Ready].status"),
            [
                Segment::Field("status".into()),
                Segment::Field("conditions".into()),
                Segment::Filter("type".into(), "Ready".into()),
                Segment::Field("status".into())
            ]
        );
        assert_eq!(path("spec.containers[0].image")[2], Segment::Index(0));
        assert_eq!(path(r#"metadata.labels["a.b/c"]"#)[2], Segment::Field("a.b/c".into()));
        assert_eq!(path("[0]"), [Segment::Index(0)]);
        assert!(parse_path("").is_err());
        assert!(parse_path("a[").is_err());
        assert!(parse_path("a[x]").is_err());
    }

    #[test]
    fn looks_values_up() {
        let m = json!({"status": {"phase": "Running", "restarts": 3, "conditions": [
            {"type": "Initialized", "status": "True"}, {"type": "Ready", "status": "False"}],
            "containerStatuses": [{"restartCount": 7}]}, "metadata": {"labels": {"a.b/c": "x"}}});
        let get = |p: &str| lookup(&m, &path(p)).cloned();
        assert_eq!(get("status.phase"), Some(json!("Running")));
        assert_eq!(get("status.conditions[type=Ready].status"), Some(json!("False")));
        assert_eq!(get("status.containerStatuses[0].restartCount"), Some(json!(7)));
        assert_eq!(get(r#"metadata.labels["a.b/c"]"#), Some(json!("x")));
        assert_eq!(get("status.conditions[type=Nope].status"), None);
        assert_eq!(get("status.containerStatuses[1]"), None);
        assert_eq!(get("status.phase.deeper"), None);
    }

    #[test]
    fn conditions() {
        let m = json!({"n": 2, "s": "hello world", "list": ["a", 1], "nothing": null});
        let holds = |c: Value| cond_holds(&cond(c), &m);
        assert!(holds(json!({"path": "n", "equals": 2})));
        assert!(holds(json!({"path": "n", "equals": 2.0})));
        assert!(!holds(json!({"path": "n", "equals": 3})));
        assert!(holds(json!({"path": "n", "notEquals": 3})));
        assert!(holds(json!({"path": "missing", "notEquals": 3})));
        assert!(!holds(json!({"path": "missing", "equals": 3})));
        assert!(holds(json!({"path": "n", "atLeast": 2, "atMost": 2})));
        assert!(!holds(json!({"path": "n", "atLeast": 3})));
        assert!(!holds(json!({"path": "s", "atLeast": 1})));
        assert!(holds(json!({"path": "s", "contains": "lo wo"})));
        assert!(holds(json!({"path": "list", "contains": 1})));
        assert!(!holds(json!({"path": "list", "contains": "z"})));
        assert!(holds(json!({"path": "n", "exists": true})));
        assert!(holds(json!({"path": "missing", "exists": false})));
        assert!(
            holds(json!({"path": "nothing", "exists": false})),
            "null counts as absent"
        );
        assert!(!holds(json!({"path": "n", "exists": false})));
        assert!(holds(json!({"path": "n", "equals": 2, "atLeast": 1})));
        assert!(!holds(json!({"path": "n", "equals": 2, "atLeast": 5})));
    }

    #[test]
    fn counts() {
        let c = |v: Value| Some(serde_json::from_value::<Count>(v).unwrap());
        assert!(count_ok(&None, 1, 1) && !count_ok(&None, 0, 1) && count_ok(&None, 0, 0));
        assert!(count_ok(&c(json!({"exactly": 1})), 1, 1) && !count_ok(&c(json!({"exactly": 1})), 2, 1));
        assert!(count_ok(&c(json!({"min": 2})), 5, 1) && !count_ok(&c(json!({"min": 2})), 1, 1));
        assert!(count_ok(&c(json!({"max": 2})), 0, 1) && !count_ok(&c(json!({"max": 2})), 3, 1));
        assert!(count_ok(&c(json!({"min": 1, "max": 2})), 2, 1));
    }

    fn store() -> ClusterStore {
        let store = ClusterStore::new().unwrap();
        for (name, app, ready) in [("a", "web", "True"), ("b", "web", "False"), ("c", "db", "True")] {
            let pod = json!({"metadata": {"name": name, "labels": {"app": app}},
                "status": {"conditions": [{"type": "Ready", "status": ready}]}});
            store.insert("Pod", "v1", "default", name, &pod).unwrap();
        }
        store
    }

    fn rule(value: Value) -> Rule {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn rules_select_by_kind_name_and_labels() {
        let s = store();
        let all_ready = rule(
            json!({"label": "x", "select": {"kind": "Pod", "labels": {"app": "web"}},
            "each": [{"path": "status.conditions[type=Ready].status", "equals": "True"}]}),
        );
        assert!(!holds(&all_ready, &s, 1).unwrap(), "pod b is not ready");
        let db = rule(json!({"label": "x", "select": {"kind": "Pod", "labels": {"app": "db"}},
            "each": [{"path": "status.conditions[type=Ready].status", "equals": "True"}]}));
        assert!(holds(&db, &s, 1).unwrap());
        let by_name = rule(json!({"label": "x", "select": {"kind": "Pod", "name": "a"}, "count": {"exactly": 1}}));
        assert!(holds(&by_name, &s, 1).unwrap());
        let missing = rule(json!({"label": "x", "select": {"kind": "Pod", "name": "zzz"}}));
        assert!(
            !holds(&missing, &s, 1).unwrap(),
            "assertions need at least one object by default"
        );
        assert!(
            holds(&missing, &s, 0).unwrap(),
            "constraints are vacuously true without objects"
        );
        let other_ns =
            rule(json!({"label": "x", "select": {"kind": "Pod", "namespace": "ghost"}, "count": {"exactly": 0}}));
        assert!(holds(&other_ns, &s, 1).unwrap());
    }

    fn matcher(v: Value) -> CommandMatcher {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn matches_parsed_commands() {
        let m = |v: Value, line: &str| matches_command(&matcher(v), &parse(line).unwrap(), &[]);
        assert!(m(
            json!({"verb": "logs", "name": "payments-api*"}),
            "kubectl logs payments-api-abc-123"
        ));
        assert!(m(
            json!({"verb": "logs", "name": "payments-api*"}),
            "kubectl logs deploy/payments-api"
        ));
        assert!(!m(
            json!({"verb": "logs", "name": "payments-api*"}),
            "kubectl logs web-1"
        ));
        assert!(m(
            json!({"verb": "logs", "resource": "deploy"}),
            "kubectl logs deploy/web"
        ));
        assert!(!m(json!({"verb": "logs", "resource": "deploy"}), "kubectl logs web-1"));
        assert!(m(
            json!({"verb": "get", "resource": "events"}),
            "kubectl get ev -n default"
        ));
        assert!(!m(json!({"verb": "get", "resource": "events"}), "kubectl get pods"));
        assert!(m(json!({"verb": "get"}), "kubectl get pods"));
        assert!(m(
            json!({"verb": "describe", "name": "payments-api*"}),
            "kubectl describe pod payments-api-1"
        ));
        assert!(!m(
            json!({"verb": "describe", "name": "payments-api*"}),
            "kubectl describe pods"
        ));
        assert!(m(
            json!({"verb": "set-env", "name": "payments-api", "list": true}),
            "kubectl set env deploy/payments-api --list"
        ));
        assert!(!m(
            json!({"verb": "set-env", "list": true}),
            "kubectl set env deploy/payments-api A=B"
        ));
        assert!(m(
            json!({"verb": "set-env", "list": false}),
            "kubectl set env deploy/payments-api A=B"
        ));
        assert!(m(
            json!({"verb": "rollout-restart", "name": "web"}),
            "kubectl rollout restart deploy/web"
        ));
        assert!(m(json!({"verb": "delete", "resource": "pods"}), "kubectl delete pod x"));
        assert!(!m(json!({"verb": "delete"}), "kubectl get pods"));
    }

    #[test]
    fn error_output_is_recognised() {
        assert!(is_error_output("error: unknown flag"));
        assert!(is_error_output("Error from server (NotFound): pods \"x\" not found"));
        assert!(is_error_output("The Pod \"x\" is invalid: spec: Forbidden"));
        assert!(!is_error_output("NAME   READY"));
        assert!(!is_error_output(""));
    }

    fn rules(value: Value) -> Result<Rules, String> {
        let parsed: Value = value;
        let success = parsed
            .get("success")
            .map(|s| serde_json::from_value(s.clone()).map_err(|e| e.to_string()))
            .transpose()?;
        let findings = parsed
            .get("findings")
            .map(|f| serde_json::from_value(f.clone()).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or_default();
        let r = Rules { success, findings };
        r.validate()?;
        Ok(r)
    }

    fn ok_assertion() -> Value {
        json!({"label": "a", "select": {"kind": "Pod"}, "each": [{"path": "status.phase", "equals": "Running"}]})
    }

    #[test]
    fn accepts_a_valid_rule_set() {
        let r = rules(json!({
            "success": {"stableSeconds": 5, "assert": [ok_assertion()],
                "constraints": [ok_assertion(), {"label": "c", "forbid": {"verb": "delete"}}]},
            "findings": [{"id": "f", "clue": "c", "when": {"anyOf": [{"command": {"verb": "logs"}}]}}]
        }))
        .unwrap();
        assert_eq!(r.success.unwrap().stable_seconds, 5);
        assert_eq!(
            rules(json!({"success": {"assert": [ok_assertion()]}}))
                .unwrap()
                .success
                .unwrap()
                .stable_seconds,
            15
        );
        assert!(rules(json!({})).is_ok());
    }

    #[test]
    fn rejects_mistakes_with_helpful_messages() {
        let err = |v: Value| rules(v).unwrap_err();
        let success = |assertions: Value, constraints: Value| json!({"success": {"assert": assertions, "constraints": constraints}});

        assert!(err(success(json!([]), json!([]))).contains("at least one assertion"));
        assert!(
            err(success(json!([{"label": "a", "select": {"kind": "Pods"}}]), json!([])))
                .contains("unknown kind \"Pods\"")
        );
        assert!(err(success(json!([{"label": "a"}]), json!([]))).contains("needs a `select`"));
        assert!(
            err(success(json!([{"label": "a", "forbid": {"verb": "get"}}]), json!([])))
                .contains("only allowed in constraints")
        );
        assert!(err(success(json!([ok_assertion()]), json!([{"label": "c"}]))).contains("exactly one of"));
        assert!(err(success(
            json!([ok_assertion()]),
            json!([{"label": "c", "forbid": {"verb": "nuke"}}])
        ))
        .contains("unknown verb"));
        assert!(err(success(
            json!([{"label": "a", "select": {"kind": "Pod"}, "each": [{"path": "x"}]}]),
            json!([])
        ))
        .contains("needs one of"));
        assert!(err(success(
            json!([{"label": "a", "select": {"kind": "Pod"}, "each": [{"path": "a[", "equals": 1}]}]),
            json!([])
        ))
        .contains("unclosed"));
        assert!(
            err(success(json!([{"label": " ", "select": {"kind": "Pod"}}]), json!([]))).contains("needs a `label`")
        );
        assert!(err(json!({"success": {"assert": [ok_assertion()], "stableSeconds": -1}})).contains("negative"));
        // Typos in field names are errors, not silently ignored.
        assert!(
            err(json!({"success": {"assert": [{"label": "a", "select": {"kind": "Pod"}, "evry": []}]}}))
                .contains("unknown field")
        );

        let finding = |id: &str, when: Value| json!({"findings": [{"id": id, "clue": "c", "when": when}]});
        assert!(err(finding("", json!({"command": {"verb": "get"}}))).contains("non-empty and unique"));
        assert!(err(finding("f", json!({}))).contains("exactly one of"));
        assert!(err(finding("f", json!({"anyOf": []}))).contains("exactly one of"));
        assert!(
            err(finding("f", json!({"command": {"verb": "get", "resource": "widgets"}}))).contains("unknown resource")
        );
        assert!(err(json!({"findings": [
            {"id": "f", "clue": "c", "when": {"command": {"verb": "get"}}},
            {"id": "f", "clue": "d", "when": {"command": {"verb": "get"}}}]}))
        .contains("unique"));
        assert!(
            err(json!({"findings": [{"id": "f", "clue": " ", "when": {"command": {"verb": "get"}}}]}))
                .contains("needs a `clue`")
        );
    }

    #[test]
    fn every_registered_kind_and_file_verb_is_accepted() {
        for kind in ["Node", "Namespace", "ConfigMap", "Secret", "Service"] {
            assert!(
                rules(json!({"success": {"assert": [{"label": "a", "select": {"kind": kind}}]}})).is_ok(),
                "{kind}"
            );
        }
        let err = rules(json!({"success": {"assert": [{"label": "a", "select": {"kind": "Pods"}}]}})).unwrap_err();
        assert!(
            err.contains("(expected ConfigMap, Deployment, Event, Namespace, Node, Pod"),
            "{err}"
        );
        for verb in ["apply", "create", "delete"] {
            let finding = json!({"findings": [{"id": "f", "clue": "c",
                "when": {"command": {"verb": verb, "resource": "cm", "name": "dossier"}}}]});
            assert!(rules(finding).is_ok(), "{verb}");
        }
    }

    #[test]
    fn cluster_scoped_kinds_are_selected_without_a_namespace() {
        let store = ClusterStore::new().unwrap();
        crate::api::seed_system_namespaces(&store, 0).unwrap();
        let rule: Rule = serde_json::from_value(json!({"label": "homicide exists",
            "select": {"kind": "Namespace", "name": "homicide"}}))
        .unwrap();
        assert!(!holds(&rule, &store, 1).unwrap());
        crate::api::create(
            &store,
            &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "homicide"}}),
            "default",
            0,
            crate::api::Origin::Client,
        )
        .unwrap();
        assert!(holds(&rule, &store, 1).unwrap());
    }

    #[test]
    fn file_commands_match_the_objects_they_touched() {
        let m =
            |v: Value, line: &str, touched: &[Touched]| matches_command(&matcher(v), &parse(line).unwrap(), touched);
        let web = [("Deployment", "web".to_string())];
        assert!(m(
            json!({"verb": "apply", "resource": "deploy", "name": "web"}),
            "kubectl apply -f a.yaml",
            &web
        ));
        assert!(m(
            json!({"verb": "apply", "name": "w*"}),
            "kubectl apply -f a.yaml",
            &web
        ));
        assert!(!m(
            json!({"verb": "apply", "resource": "pods"}),
            "kubectl apply -f a.yaml",
            &web
        ));
        assert!(!m(json!({"verb": "apply"}), "kubectl apply -f a.yaml", &[]));
        assert!(!m(json!({"verb": "create"}), "kubectl apply -f a.yaml", &web));
        assert!(m(
            json!({"verb": "create", "resource": "deployments.apps"}),
            "kubectl create -f a.yaml",
            &web
        ));
        assert!(m(
            json!({"verb": "delete", "resource": "deployment", "name": "web"}),
            "kubectl delete -f a.yaml",
            &web
        ));
        assert!(m(
            json!({"verb": "delete", "resource": "deployment", "name": "web"}),
            "kubectl delete deploy web",
            &[]
        ));
    }

    #[test]
    fn a_partly_failed_file_delete_still_breaks_a_forbid() {
        let mut j = judge(json!([{"label": "keep web",
            "forbid": {"verb": "delete", "resource": "deployment", "name": "web"}}]));
        j.record_command(
            &parse("kubectl delete -f all.yaml").unwrap(),
            &[("Deployment", "web".to_string())],
            "deployment.apps \"web\" deleted\nError from server (NotFound): error when deleting \"all.yaml\": pods \"x\" not found",
            1001,
        );
        j.observe(&pod("Running"), 1002);
        j.observe(&pod("Running"), 1012);
        assert_eq!(j.status(1012).outcome, Some("closedWithCost"));

        let mut j = judge(json!([{"label": "keep web", "forbid": {"verb": "delete", "name": "web"}}]));
        j.record_command(
            &parse("kubectl delete -f all.yaml").unwrap(),
            &[],
            "Error from server (NotFound): error when deleting \"all.yaml\": deployments.apps \"web\" not found",
            1001,
        );
        j.observe(&pod("Running"), 1002);
        j.observe(&pod("Running"), 1012);
        assert_eq!(j.status(1012).outcome, Some("closed"));
    }

    // ---- the state machine, driven directly --------------------------------------------------

    fn judge(extra_constraints: Value) -> Judge {
        let mut j = Judge::new(
            rules(json!({
                "success": {"stableSeconds": 10,
                    "assert": [{"label": "a is ready", "select": {"kind": "Pod", "name": "a"},
                        "each": [{"path": "status.phase", "equals": "Running"}]}],
                    "constraints": extra_constraints},
                "findings": [{"id": "look", "clue": "you looked", "when": {"command": {"verb": "logs"}}}]
            }))
            .unwrap(),
        );
        j.start(1000);
        j
    }

    fn pod(phase: &str) -> ClusterStore {
        let s = ClusterStore::new().unwrap();
        s.insert(
            "Pod",
            "v1",
            "default",
            "a",
            &json!({"metadata": {"name": "a"}, "status": {"phase": phase}}),
        )
        .unwrap();
        s
    }

    #[test]
    fn solves_only_after_the_state_has_held_for_the_whole_window() {
        let mut j = judge(json!([]));
        j.observe(&pod("Pending"), 1000);
        assert!(!j.status(1000).solved);
        assert_eq!(j.next_wake(), None);

        let running = pod("Running");
        j.observe(&running, 1005);
        assert_eq!(j.next_wake(), Some(1015));
        j.observe(&running, 1014);
        assert!(!j.status(1014).solved);
        j.observe(&running, 1015);
        let status = j.status(1015);
        assert!(status.solved);
        assert_eq!(
            (status.outcome, status.solved_at_seconds, status.elapsed_seconds),
            (Some("closed"), Some(15), 15)
        );
        assert_eq!(j.next_wake(), None);
    }

    #[test]
    fn a_flapping_state_resets_the_window() {
        let mut j = judge(json!([]));
        j.observe(&pod("Running"), 1000);
        j.observe(&pod("Pending"), 1008);
        assert_eq!(j.next_wake(), None);
        j.observe(&pod("Running"), 1009);
        j.observe(&pod("Running"), 1018);
        assert!(!j.status(1018).solved);
        j.observe(&pod("Running"), 1019);
        assert_eq!(j.status(1019).solved_at_seconds, Some(19));
    }

    #[test]
    fn solving_is_final() {
        let mut j = judge(json!([]));
        j.observe(&pod("Running"), 1000);
        j.observe(&pod("Running"), 1010);
        let before = serde_json::to_string(&j.status(1010)).unwrap();
        j.observe(&pod("Failed"), 1500);
        j.record_command(&parse("kubectl logs a").unwrap(), &[], "x", 1501);
        let after = j.status(1500);
        assert!(after.solved && after.findings.found.is_empty());
        assert_eq!(after.solved_at_seconds, Some(10));
        assert_eq!(after.elapsed_seconds, 10);
        assert_ne!(before, "", "status serializes");
    }

    #[test]
    fn broken_constraints_are_sticky_and_change_the_outcome() {
        let mut j =
            judge(json!([{"label": "pod b must exist", "select": {"kind": "Pod", "name": "b"}, "count": {"min": 1}}]));
        let s = pod("Running");
        s.insert("Pod", "v1", "default", "b", &json!({"metadata": {"name": "b"}}))
            .unwrap();
        j.observe(&s, 1000);
        s.delete("Pod", "default", "b").unwrap();
        j.observe(&s, 1003);
        s.insert("Pod", "v1", "default", "b", &json!({"metadata": {"name": "b"}}))
            .unwrap();
        j.observe(&s, 1010);
        j.observe(&s, 1013);
        let status = j.status(1013);
        assert_eq!(status.outcome, Some("closedWithCost"));
        let report = status.report.unwrap();
        assert_eq!(report.constraints[0].broken_at_seconds, Some(3));
        assert!(!report.constraints[0].ok);
        assert!(report.assertions[0].ok);
    }

    #[test]
    fn forbidden_commands_break_a_constraint_only_when_they_succeed() {
        let mut j = judge(json!([{"label": "never delete", "forbid": {"verb": "delete"}}]));
        j.record_command(
            &parse("kubectl delete pod x").unwrap(),
            &[],
            "Error from server (NotFound): pods \"x\" not found",
            1001,
        );
        j.observe(&pod("Running"), 1002);
        j.observe(&pod("Running"), 1012);
        assert_eq!(j.status(1012).outcome, Some("closed"));

        let mut j = judge(json!([{"label": "never delete", "forbid": {"verb": "delete"}}]));
        j.record_command(&parse("kubectl delete pod x").unwrap(), &[], "pod \"x\" deleted", 1001);
        j.observe(&pod("Running"), 1002);
        j.observe(&pod("Running"), 1012);
        assert_eq!(j.status(1012).outcome, Some("closedWithCost"));
    }

    #[test]
    fn findings_unlock_once_from_successful_commands_and_report_when() {
        let mut j = judge(json!([]));
        j.record_command(
            &parse("kubectl logs nope").unwrap(),
            &[],
            "Error from server (NotFound): pods \"nope\" not found",
            1003,
        );
        assert!(j.status(1003).findings.found.is_empty());
        assert_eq!(j.status(1003).commands, 1);
        j.record_command(&parse("kubectl get pods").unwrap(), &[], "NAME", 1004);
        assert!(j.status(1004).findings.found.is_empty());
        j.record_command(&parse("kubectl logs a").unwrap(), &[], "line", 1007);
        j.record_command(&parse("kubectl logs a").unwrap(), &[], "line", 1009);
        let status = j.status(1009);
        assert_eq!(status.findings.total, 1);
        assert_eq!(status.findings.found.len(), 1);
        assert_eq!(
            (
                status.findings.found[0].id.as_str(),
                status.findings.found[0].at_seconds
            ),
            ("look", 7)
        );
        assert_eq!(status.commands, 4);
        assert!(status.report.is_none(), "no report before the case is closed");
    }

    #[test]
    fn nothing_counts_before_the_player_starts() {
        let mut j = Judge::new(Rules::default());
        j.record_command(&parse("kubectl get pods").unwrap(), &[], "x", 1);
        assert_eq!(j.status(1).commands, 0);
        let mut j = judge(json!([]));
        j.started_at = None;
        j.observe(&pod("Running"), 5000);
        assert_eq!(j.next_wake(), None);
    }

    #[test]
    fn an_env_assertion_tells_a_wrong_value_from_the_right_one() {
        let assertion = rule(
            json!({"label": "configured", "select": {"kind": "Deployment", "name": "api"},
            "each": [{"path": "spec.template.spec.containers[0].env[name=DATABASE_URL].value", "equals": "postgres://right"}]}),
        );
        let store_with = |env: Value| {
            let s = ClusterStore::new().unwrap();
            let d =
                json!({"metadata": {"name": "api"}, "spec": {"template": {"spec": {"containers": [{"env": env}]}}}});
            s.insert("Deployment", "apps/v1", "default", "api", &d).unwrap();
            s
        };
        let var = |name: &str, value: &str| json!({"name": name, "value": value});
        assert!(holds(
            &assertion,
            &store_with(json!([var("A", "1"), var("DATABASE_URL", "postgres://right")])),
            1
        )
        .unwrap());
        assert!(!holds(
            &assertion,
            &store_with(json!([var("DATABASE_URL", "postgres://wrong")])),
            1
        )
        .unwrap());
        assert!(
            !holds(
                &assertion,
                &store_with(json!([var("DATABSE_URL", "postgres://right")])),
                1
            )
            .unwrap(),
            "wrong name"
        );
        assert!(!holds(&assertion, &store_with(json!([])), 1).unwrap());
        assert!(!holds(&assertion, &store_with(json!(null)), 1).unwrap());
    }

    #[test]
    fn without_success_rules_the_case_never_closes() {
        let mut j = Judge::new(Rules::default());
        j.start(0);
        j.observe(&pod("Running"), 100);
        assert!(!j.status(100).solved);
        assert_eq!(j.next_wake(), None);
    }
}
