//! kubectl command-line parsing. Pure Rust with no wasm-bindgen types.

use std::fmt;

use crate::selector::Selector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Pod,
    Event,
    Deployment,
    ReplicaSet,
    Job,
}

impl ResourceKind {
    pub(crate) fn from_arg(arg: &str) -> Result<Self, ParseError> {
        match arg {
            "pods" | "pod" | "po" => Ok(ResourceKind::Pod),
            "events" | "event" | "ev" => Ok(ResourceKind::Event),
            "deployments" | "deployment" | "deploy" => Ok(ResourceKind::Deployment),
            "replicasets" | "replicaset" | "rs" => Ok(ResourceKind::ReplicaSet),
            "jobs" | "job" => Ok(ResourceKind::Job),
            other => Err(unsupported_resource(other)),
        }
    }

    /// [`from_arg`](Self::from_arg) for a command: a kind the API knows but this command cannot
    /// act on says so, rather than claiming the server lacks the resource.
    pub(crate) fn for_verb(verb: &str, arg: &str) -> Result<Self, ParseError> {
        Self::from_arg(arg).map_err(|error| match (&error, crate::api::kind_for_arg(arg)) {
            (ParseError::UnsupportedResource(_), Some(info)) => {
                ParseError::Unsupported(format!("kubectl {verb} for {}", info.resource()))
            }
            _ => error,
        })
    }

    /// Kind as stored in the cluster (`Pod`).
    pub fn kind(self) -> &'static str {
        match self {
            ResourceKind::Pod => "Pod",
            ResourceKind::Event => "Event",
            ResourceKind::Deployment => "Deployment",
            ResourceKind::ReplicaSet => "ReplicaSet",
            ResourceKind::Job => "Job",
        }
    }

    /// Resource name used in kubectl messages (`pods`, `deployments.apps`).
    pub fn plural(self) -> &'static str {
        match self {
            ResourceKind::Pod => "pods",
            ResourceKind::Event => "events",
            ResourceKind::Deployment => "deployments.apps",
            ResourceKind::ReplicaSet => "replicasets.apps",
            ResourceKind::Job => "jobs.batch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Wide,
    Json,
    Yaml,
    Name,
    /// `-o jsonpath=TEMPLATE`
    JsonPath(String),
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, ParseError> {
        match value {
            "" => Ok(OutputFormat::Table),
            "wide" => Ok(OutputFormat::Wide),
            "json" => Ok(OutputFormat::Json),
            "yaml" => Ok(OutputFormat::Yaml),
            "name" => Ok(OutputFormat::Name),
            "jsonpath" => Err(ParseError::MissingTemplate),
            other => match other.strip_prefix("jsonpath=") {
                Some(template) => Ok(OutputFormat::JsonPath(template.to_string())),
                None => Err(ParseError::UnknownOutputFormat(
                    other.split('=').next().unwrap_or(other).to_string(),
                )),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetArgs {
    /// The kind to list, as the API registry names it (`Pod`, `ConfigMap`).
    pub kind: &'static str,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    pub output: OutputFormat,
    /// `-l`: only objects whose labels match.
    pub selector: Selector,
    /// `-A`: every namespace.
    pub all_namespaces: bool,
    /// `--no-headers`
    pub no_headers: bool,
    /// `--show-labels`
    pub show_labels: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribeArgs {
    /// The kind to describe, as the API registry names it (`Pod`, `Namespace`).
    pub kind: &'static str,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    /// `-l`: only the objects whose labels match.
    pub selector: Selector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsArgs {
    /// `Pod`, or `Deployment` to read the logs of one of its pods.
    pub resource: ResourceKind,
    pub name: String,
    pub namespace: Option<String>,
    pub container: Option<String>,
    pub previous: bool,
    /// `None` shows everything; `Some(n)` the last `n` lines.
    pub tail: Option<usize>,
    pub timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvChange {
    Set { name: String, value: String },
    Unset(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEnvArgs {
    pub resource: ResourceKind,
    pub name: String,
    pub namespace: Option<String>,
    /// Restrict to one container; `None` means all of them.
    pub container: Option<String>,
    /// `--list`: print the environment instead of changing it.
    pub list: bool,
    pub changes: Vec<EnvChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutRestartArgs {
    pub resource: ResourceKind,
    pub names: Vec<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteArgs {
    /// The kind to delete, as the API registry names it (`Pod`, `ConfigMap`).
    pub kind: &'static str,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    /// `--force` / `--grace-period=0` / `--now`: skip the termination grace period.
    pub immediate: bool,
}

/// `-f` files for `apply`, `create`, `delete` and `replace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestArgs {
    pub files: Vec<String>,
    pub namespace: Option<String>,
    /// `replace --force`: delete and re-create instead of updating.
    pub force: bool,
}

/// `cat FILE...` and `ls [FILE]...` over the terminal's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathArgs {
    pub paths: Vec<String>,
}

/// How `kubectl patch` reads its patch (`--type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchType {
    Strategic,
    Merge,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchArgs {
    /// The resource type as typed (any kind the API server knows; resolved when run).
    pub resource: String,
    pub name: String,
    pub namespace: Option<String>,
    pub patch: String,
    pub patch_type: PatchType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Get(GetArgs),
    Describe(DescribeArgs),
    Logs(LogsArgs),
    SetEnv(SetEnvArgs),
    RolloutRestart(RolloutRestartArgs),
    Delete(DeleteArgs),
    Apply(ManifestArgs),
    Create(ManifestArgs),
    DeleteFiles(ManifestArgs),
    Replace(ManifestArgs),
    Patch(PatchArgs),
    Cat(PathArgs),
    Ls(PathArgs),
    /// `vim FILE` / `vi FILE`: open the terminal editor on one of the terminal's own files.
    Vim(PathArgs),
    /// `kubectl --help` (topic `""`) or `kubectl COMMAND --help` (see `help`).
    Help(String),
    Explain(ExplainArgs),
    Run(RunArgs),
    CreateObject(CreateArgs),
    SetImage(SetImageArgs),
    Scale(ScaleArgs),
    Rollout(RolloutArgs),
    Label(LabelArgs),
    Expose(ExposeArgs),
    Exec(ExecArgs),
}

/// `exec POD [-c CONTAINER] -- COMMAND ARGS...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecArgs {
    pub pod: String,
    pub namespace: Option<String>,
    pub container: Option<String>,
    pub command: Vec<String>,
}

/// `run NAME --image=IMAGE ...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    pub name: String,
    pub image: String,
    pub namespace: Option<String>,
    pub env: Vec<(String, String)>,
    pub port: Option<i32>,
    /// `--labels`; when empty the pod gets `run=NAME`.
    pub labels: Vec<(String, String)>,
    pub restart: Option<String>,
    /// Words after `--`: the container's arguments, or its command with `--command`.
    pub trailing: Vec<String>,
    pub command: bool,
}

/// What `kubectl create KIND NAME` makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateObject {
    Namespace,
    Deployment {
        image: String,
        replicas: i32,
        port: Option<i32>,
    },
    ConfigMap {
        literals: Vec<(String, String)>,
        files: Vec<(Option<String>, String)>,
    },
    Secret {
        literals: Vec<(String, String)>,
        files: Vec<(Option<String>, String)>,
        secret_type: Option<String>,
    },
    DockerRegistrySecret {
        server: String,
        username: String,
        password: String,
        email: Option<String>,
    },
    Job {
        image: String,
        command: Vec<String>,
    },
    CronJob {
        image: String,
        schedule: String,
        command: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateArgs {
    pub name: String,
    pub namespace: Option<String>,
    pub object: CreateObject,
}

/// `set image TYPE/NAME CONTAINER=IMAGE...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetImageArgs {
    pub kind: &'static str,
    pub name: String,
    pub namespace: Option<String>,
    pub images: Vec<(String, String)>,
}

/// `scale TYPE/NAME... --replicas=N`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleArgs {
    pub kind: &'static str,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    pub replicas: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolloutAction {
    Status,
    History,
    Undo { to_revision: Option<i64> },
}

/// `rollout status|history|undo deployment/NAME`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutArgs {
    pub action: RolloutAction,
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelChange {
    Set(String, String),
    Remove(String),
}

/// `label TYPE NAME... KEY=VALUE... KEY-`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelArgs {
    pub kind: &'static str,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    pub changes: Vec<LabelChange>,
    pub overwrite: bool,
}

/// `expose TYPE/NAME [--port ...]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposeArgs {
    pub kind: &'static str,
    pub name: String,
    pub namespace: Option<String>,
    pub port: Option<i32>,
    pub target_port: Option<String>,
    pub service_name: Option<String>,
    pub service_type: Option<String>,
    pub protocol: Option<String>,
}

/// `explain RESOURCE[.FIELD...]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainArgs {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    UnknownCommand(String),
    MissingVerb,
    UnknownVerb(String),
    /// The verb that needed a resource type (`get`, `describe`).
    MissingResource(&'static str),
    UnsupportedResource(String),
    MixedResourceForms,
    MissingValue(&'static str),
    UnknownFlag(String),
    UnknownOutputFormat(String),
    InvalidInt {
        flag: &'static str,
        value: String,
    },
    LogsMissingPod,
    LogsUnsupportedTarget(String),
    UnterminatedQuote,
    /// A subcommand the simulation does not know, e.g. `kubectl set image`.
    UnknownSubcommand {
        parent: &'static str,
        name: String,
    },
    MissingSubcommand(&'static str),
    MissingName,
    InvalidEnvSpec(String),
    SetEnvNoChanges,
    /// `apply` / `create` without `-f`.
    MissingFilename,
    /// Both `-f` and resource arguments.
    FilesAndResources,
    /// A real kubectl feature this terminal does not offer.
    Unsupported(String),
    UnexpectedArgs(Vec<String>),
    /// `-o jsonpath` without `=TEMPLATE`.
    MissingTemplate,
    InvalidSelector(String),
    /// `get -A NAME`
    NameAcrossNamespaces,
    /// `patch` without `-p`.
    MissingPatch,
    InvalidPatchType(String),
    /// `--force` outside `replace`.
    ForceWithoutReplace,
    /// A required flag was not given (`error: required flag(s) "image" not set`).
    MissingFlag(&'static str),
    /// The boss's answer to something this precinct does not do; printed as is.
    Boss(&'static str),
    /// A failure of a built-in shell command, already worded (`ls: invalid option -- 'l'`).
    Builtin(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "error: no command given"),
            ParseError::UnknownCommand(c) => write!(f, "{c}: command not found"),
            ParseError::MissingVerb => write!(f, "error: You must specify the type of command"),
            ParseError::UnknownVerb(v) => write!(f, "error: unknown command \"{v}\" for \"kubectl\""),
            ParseError::MissingResource(verb) => write!(
                f,
                "error: You must specify the type of resource to {verb}. Use \"kubectl api-resources\" for a complete list of supported resources."
            ),
            ParseError::UnsupportedResource(r) => {
                write!(f, "error: the server doesn't have a resource type \"{r}\"")
            }
            ParseError::MixedResourceForms => write!(
                f,
                "error: there is no need to specify a resource type as a separate argument when passing arguments in resource/name form (e.g. 'kubectl get resource/<resource_name>' instead of 'kubectl get resource resource/<resource_name>')"
            ),
            ParseError::MissingValue(flag) => write!(f, "error: flag needs an argument: {flag}"),
            ParseError::UnknownFlag(flag) if flag.starts_with("--") => {
                write!(f, "error: unknown flag: {flag}")
            }
            ParseError::UnknownFlag(flag) => {
                let short = flag.chars().nth(1).unwrap_or('-');
                write!(f, "error: unknown shorthand flag: '{short}' in {flag}")
            }
            ParseError::UnknownOutputFormat(format) => write!(
                f,
                "error: unable to match a printer suitable for the output format \"{format}\", allowed formats are: json,jsonpath,name,wide,yaml"
            ),
            ParseError::InvalidInt { flag, value } => write!(
                f,
                "error: invalid argument \"{value}\" for \"{flag}\" flag: strconv.ParseInt: parsing \"{value}\": invalid syntax"
            ),
            ParseError::LogsMissingPod => write!(
                f,
                "error: expected 'logs [-f] [-p] (POD | TYPE/NAME) [-c CONTAINER]'.\nPOD or TYPE/NAME is a required argument for the logs command\nSee 'kubectl logs -h' for help and examples"
            ),
            ParseError::LogsUnsupportedTarget(kind) => write!(f, "error: cannot get the logs from {kind}"),
            ParseError::UnterminatedQuote => write!(f, "error: unterminated quoted string"),
            ParseError::UnknownSubcommand { parent, name } => {
                write!(f, "error: unknown command \"{name}\" for \"kubectl {parent}\"")
            }
            ParseError::MissingSubcommand(parent) => {
                write!(f, "error: You must specify a subcommand for \"kubectl {parent}\"")
            }
            ParseError::MissingName => write!(f, "error: resource(s) were provided, but no name was specified"),
            ParseError::InvalidEnvSpec(spec) => write!(f, "error: invalid environment variable \"{spec}\""),
            ParseError::SetEnvNoChanges => write!(f, "error: at least one environment variable must be provided"),
            ParseError::MissingFilename => write!(f, "error: must specify one of -f and -k"),
            ParseError::FilesAndResources => {
                write!(f, "error: resource names and -f cannot be used together")
            }
            ParseError::Unsupported(what) => write!(f, "error: {what} is not available in this terminal"),
            ParseError::UnexpectedArgs(args) => write!(f, "error: Unexpected args: [{}]", args.join(" ")),
            ParseError::MissingTemplate => write!(f, "error: template format specified but no template given"),
            ParseError::InvalidSelector(message) => write!(f, "error: {message}"),
            ParseError::NameAcrossNamespaces => {
                write!(f, "error: a resource cannot be retrieved by name across all namespaces")
            }
            ParseError::MissingPatch => write!(f, "error: must specify -p to patch"),
            ParseError::InvalidPatchType(t) => write!(
                f,
                "error: --type must be one of [json merge strategic], not \"{t}\""
            ),
            ParseError::Builtin(message) => write!(f, "{message}"),
            ParseError::Boss(message) => write!(f, "{message}"),
            ParseError::MissingFlag(flag) => write!(f, "error: required flag(s) \"{flag}\" not set"),
            ParseError::ForceWithoutReplace => write!(f, "error: unknown flag: --force"),
        }
    }
}

/// Split a command line like a POSIX shell: whitespace separates words, single and double
/// quotes group (and are removed), a backslash escapes the next character.
pub(crate) fn shell_split(input: &str) -> Result<Vec<String>, ParseError> {
    Ok(shell_split_marked(input)?.into_iter().map(|(word, _)| word).collect())
}

/// [`shell_split`], also saying which words contain an unquoted wildcard (`*`, `?`, `[`), the
/// ones a shell would expand against the files.
pub(crate) fn shell_split_marked(input: &str) -> Result<Vec<(String, bool)>, ParseError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut wildcard = false;
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars();

    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => current.extend(chars.next()),
            Some(_) => current.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    current.extend(chars.next());
                    in_word = true;
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push((std::mem::take(&mut current), std::mem::take(&mut wildcard)));
                        in_word = false;
                    }
                }
                c => {
                    wildcard |= matches!(c, '*' | '?' | '[');
                    current.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(ParseError::UnterminatedQuote);
    }
    if in_word {
        words.push((current, wildcard));
    }
    Ok(words)
}

// The terminal parses through `parse_words` (after wildcard expansion); this is for tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse(input: &str) -> Result<Command, ParseError> {
    parse_words(&shell_split(input)?)
}

/// Parse a command line already split into words (wildcards expanded).
pub fn parse_words(words: &[String]) -> Result<Command, ParseError> {
    let mut tokens = words.iter().map(String::as_str);

    match tokens.next() {
        None => return Err(ParseError::Empty),
        Some("kubectl") => {}
        Some(builtin @ ("cat" | "ls")) => return parse_paths(builtin, tokens.collect()),
        Some(editor @ ("vim" | "vi")) => return parse_vim(editor, tokens.collect()),
        Some(other) => return Err(ParseError::UnknownCommand(other.to_string())),
    }

    // `kubectl -n NS VERB ...`: the namespace flag may come before the verb.
    let mut namespace: Option<String> = None;
    let mut next = tokens.next();
    while let Some(token) = next.filter(|t| is_flag(t) && !matches!(*t, "--help" | "-h")) {
        match take_namespace(token, &mut tokens)? {
            Some(ns) => namespace = Some(ns),
            None => return Err(unknown_flag(token)),
        }
        next = tokens.next();
    }
    let verb = next.ok_or(ParseError::MissingVerb)?;
    let mut rest: Vec<&str> = tokens.collect();
    if let Some(ns) = namespace.as_deref() {
        let takes_namespace = crate::help::flags_of(verb).contains(&"-n");
        let has_own = rest.iter().any(|a| a.starts_with("-n") || a.starts_with("--namespace"));
        if takes_namespace && !has_own {
            rest.extend(["-n", ns]);
        }
    }
    if matches!(verb, "--help" | "-h") {
        return Ok(Command::Help(String::new()));
    }
    if verb == "help" {
        return parse_help_topic(&rest);
    }
    if rest.iter().any(|a| matches!(*a, "--help" | "-h")) {
        return help_for(verb, &rest);
    }

    match verb {
        "get" => parse_get(&rest).map(Command::Get),
        "describe" => parse_describe(&rest).map(Command::Describe),
        "logs" => parse_logs(&rest).map(Command::Logs),
        "delete" => parse_delete(&rest),
        "apply" => parse_manifest_args(&rest, false).map(Command::Apply),
        "replace" => parse_manifest_args(&rest, true).map(Command::Replace),
        "patch" => parse_patch(&rest).map(Command::Patch),
        "explain" => parse_explain(&rest).map(Command::Explain),
        "run" => parse_run(&rest).map(Command::Run),
        "scale" => parse_scale(&rest).map(Command::Scale),
        "label" => parse_label(&rest).map(Command::Label),
        "expose" => parse_expose(&rest).map(Command::Expose),
        "exec" => parse_exec(&rest).map(Command::Exec),
        "create" => match rest.split_first() {
            Some((sub, args)) if !is_flag(sub) => parse_create_object(sub, args),
            _ => parse_manifest_args(&rest, false).map(Command::Create),
        },
        "set" => match rest.split_first() {
            Some((&"env", args)) => parse_set_env(args).map(Command::SetEnv),
            Some((&"image", args)) => parse_set_image(args).map(Command::SetImage),
            Some((other, _)) => Err(unknown_subcommand("set", other, SET_UNSUPPORTED)),
            None => Err(ParseError::MissingSubcommand("set")),
        },
        "rollout" => match rest.split_first() {
            Some((&"restart", args)) => parse_rollout_restart(args).map(Command::RolloutRestart),
            Some((&"status", args)) => parse_rollout(RolloutAction::Status, args).map(Command::Rollout),
            Some((&"history", args)) => parse_rollout(RolloutAction::History, args).map(Command::Rollout),
            Some((&"undo", args)) => {
                parse_rollout(RolloutAction::Undo { to_revision: None }, args).map(Command::Rollout)
            }
            Some((other, _)) => Err(unknown_subcommand("rollout", other, ROLLOUT_UNSUPPORTED)),
            None => Err(ParseError::MissingSubcommand("rollout")),
        },
        other => Err(unknown_verb(other)),
    }
}

/// kubectl commands that exist but this terminal does not offer.
const UNSUPPORTED_VERBS: &[&str] = &[
    "alpha",
    "annotate",
    "api-resources",
    "api-versions",
    "attach",
    "auth",
    "autoscale",
    "certificate",
    "cluster-info",
    "completion",
    "config",
    "cordon",
    "cp",
    "debug",
    "diff",
    "drain",
    "events",
    "kustomize",
    "options",
    "plugin",
    "port-forward",
    "proxy",
    "taint",
    "top",
    "uncordon",
    "version",
    "wait",
];
const SET_UNSUPPORTED: &[&str] = &["resources", "selector", "serviceaccount", "subject"];
const ROLLOUT_UNSUPPORTED: &[&str] = &["pause", "resume"];
/// `kubectl create SUBCOMMAND`s that exist but are not offered.
const CREATE_UNSUPPORTED: &[&str] = &[
    "clusterrole",
    "clusterrolebinding",
    "ingress",
    "poddisruptionbudget",
    "priorityclass",
    "quota",
    "role",
    "rolebinding",
    "service",
    "serviceaccount",
    "token",
];

/// What the boss says to anyone who reaches for `kubectl edit`: state is changed by writing a
/// manifest and applying it.
pub const NO_EDITING: &str = "We don't do that here, we are not that kind of people.";

/// A verb kubectl has but we do not is "not available"; anything else is just unknown.
fn unknown_verb(verb: &str) -> ParseError {
    if verb == "edit" {
        ParseError::Boss(NO_EDITING)
    } else if UNSUPPORTED_VERBS.contains(&verb) {
        ParseError::Unsupported(format!("kubectl {verb}"))
    } else {
        ParseError::UnknownVerb(verb.to_string())
    }
}

fn unknown_subcommand(parent: &'static str, name: &str, unsupported: &[&str]) -> ParseError {
    if unsupported.contains(&name) {
        ParseError::Unsupported(format!("kubectl {parent} {name}"))
    } else {
        ParseError::UnknownSubcommand {
            parent,
            name: name.to_string(),
        }
    }
}

// ---- imperative commands ----------------------------------------------------------------------

fn parse_i32(flag: &'static str, value: &str) -> Result<i32, ParseError> {
    value.parse().map_err(|_| ParseError::InvalidInt {
        flag,
        value: value.to_string(),
    })
}

/// `KEY=VALUE` as `(key, value)`.
fn key_value(spec: &str, what: &str) -> Result<(String, String), ParseError> {
    match spec.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(ParseError::Builtin(format!("error: invalid {what}: {spec}"))),
    }
}

/// `[KEY=]PATH` for `--from-file`.
fn file_source(spec: &str) -> (Option<String>, String) {
    match spec.split_once('=') {
        Some((key, path)) => (Some(key.to_string()), path.to_string()),
        None => (None, spec.to_string()),
    }
}

/// What `scan_args` found: positionals, words after `--`, and the `-n` namespace.
type Scanned<'a> = (Vec<&'a str>, Vec<&'a str>, Option<String>);

/// Flags every one of these commands takes: pulls `-n` out, and hands anything else to `other`
/// (which returns whether it used the argument). Positionals are returned in order, with
/// everything after a `--` kept apart.
fn scan_args<'a>(
    args: &[&'a str],
    mut other: impl FnMut(&'a str, &mut std::slice::Iter<'_, &'a str>) -> Result<bool, ParseError>,
) -> Result<Scanned<'a>, ParseError> {
    let (mut positional, mut trailing, mut namespace) = (Vec::new(), Vec::new(), None);
    let mut iter = args.iter();
    while let Some(&arg) = iter.next() {
        if arg == "--" {
            trailing.extend(iter.by_ref().copied());
        } else if let Some(ns) = take_namespace(arg, &mut iter.by_ref().copied())? {
            namespace = Some(ns);
        } else if other(arg, &mut iter)? {
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg);
        }
    }
    Ok((positional, trailing, namespace))
}

/// The value of `--flag=value` or `--flag value`, if `arg` is that flag.
fn long_value(
    arg: &str,
    flag: &'static str,
    iter: &mut std::slice::Iter<'_, &str>,
) -> Result<Option<String>, ParseError> {
    take_long(arg, flag, &mut iter.by_ref().copied())
}

fn parse_run(args: &[&str]) -> Result<RunArgs, ParseError> {
    let (mut image, mut port, mut restart) = (None, None, None);
    let (mut env, mut labels) = (Vec::new(), Vec::new());
    let mut command = false;
    let (positional, trailing, namespace) = scan_args(args, |arg, iter| {
        if let Some(v) = long_value(arg, "--image", iter)? {
            image = Some(v);
        } else if let Some(v) = long_value(arg, "--env", iter)? {
            env.push(key_value(&v, "environment variable")?);
        } else if let Some(v) = long_value(arg, "--port", iter)? {
            port = Some(parse_i32("--port", &v)?);
        } else if let Some(v) = long_value(arg, "--labels", iter)? {
            for pair in v.split(',').filter(|p| !p.is_empty()) {
                labels.push(key_value(pair, "label")?);
            }
        } else if let Some(v) = long_value(arg, "--restart", iter)? {
            restart = Some(v);
        } else if arg == "--command" {
            command = true;
        } else {
            return Ok(false);
        }
        Ok(true)
    })?;
    let name = match positional.as_slice() {
        [] => return Err(ParseError::Builtin("error: NAME is required for run".into())),
        [name] => name.to_string(),
        [_, extra @ ..] => {
            return Err(ParseError::UnexpectedArgs(
                extra.iter().map(|a| a.to_string()).collect(),
            ))
        }
    };
    let image = image.ok_or(ParseError::MissingFlag("image"))?;
    if let Some(policy) = restart.as_deref() {
        if !["Always", "OnFailure", "Never"].contains(&policy) {
            return Err(ParseError::Builtin(format!(
                "error: invalid restart policy: {policy} (one of Always, OnFailure, Never)"
            )));
        }
    }
    Ok(RunArgs {
        name,
        image,
        namespace,
        env,
        port,
        labels,
        restart,
        trailing: trailing.iter().map(|w| w.to_string()).collect(),
        command,
    })
}

fn parse_exec(args: &[&str]) -> Result<ExecArgs, ParseError> {
    let mut container = None;
    let (positional, trailing, namespace) = scan_args(args, |arg, iter| {
        if let Some(v) = long_value(arg, "--container", iter)? {
            container = Some(v);
        } else if arg == "-c" {
            container = Some(flag_value(&mut iter.by_ref().copied(), "-c, --container")?.to_string());
        } else if matches!(arg, "-i" | "-t" | "-it" | "-ti" | "--stdin" | "--tty") {
        } else {
            return Ok(false);
        }
        Ok(true)
    })?;
    let pod = match positional.as_slice() {
        [] => {
            return Err(ParseError::Builtin(
                "error: expected 'exec (POD | TYPE/NAME) COMMAND [ARG1] [ARG2] ... [ARGN]'.\nPOD or TYPE/NAME is a required argument for the exec command\nSee 'kubectl exec -h' for help and examples".into(),
            ))
        }
        [pod] => pod.strip_prefix("pod/").or_else(|| pod.strip_prefix("pods/")).unwrap_or(pod).to_string(),
        [_, extra @ ..] => {
            return Err(ParseError::UnexpectedArgs(extra.iter().map(|a| a.to_string()).collect()));
        }
    };
    if trailing.is_empty() {
        return Err(ParseError::Builtin(
            "error: you must specify at least one command for the container".into(),
        ));
    }
    Ok(ExecArgs {
        pod,
        namespace,
        container,
        command: trailing.iter().map(|w| w.to_string()).collect(),
    })
}

fn parse_create_object(sub: &str, args: &[&str]) -> Result<Command, ParseError> {
    let (mut image, mut replicas, mut port, mut secret_type) = (None, 1, None, None);
    let (mut literals, mut files) = (Vec::new(), Vec::new());
    let (mut server, mut username, mut password, mut email) = (None, None, None, None);
    let mut schedule = None;
    let (kind, rest) = match sub {
        "namespace" | "ns" => ("namespace", args),
        "deployment" | "deploy" => ("deployment", args),
        "job" => ("job", args),
        "cronjob" | "cj" => ("cronjob", args),
        "configmap" | "cm" => ("configmap", args),
        "secret" => match args.split_first() {
            Some((&"generic", rest)) => ("secret generic", rest),
            Some((&"docker-registry", rest)) => ("secret docker-registry", rest),
            Some((&"tls", _)) => return Err(ParseError::Unsupported("kubectl create secret tls".into())),
            Some((other, _)) if !is_flag(other) => {
                return Err(ParseError::UnknownSubcommand {
                    parent: "create secret",
                    name: other.to_string(),
                })
            }
            _ => return Err(ParseError::MissingSubcommand("create secret")),
        },
        other if CREATE_UNSUPPORTED.contains(&other) => {
            return Err(ParseError::Unsupported(format!("kubectl create {other}")))
        }
        other => {
            return Err(ParseError::UnknownSubcommand {
                parent: "create",
                name: other.to_string(),
            })
        }
    };
    let (positional, trailing, namespace) = scan_args(rest, |arg, iter| {
        if let Some(v) = long_value(arg, "--image", iter)? {
            image = Some(v);
        } else if let Some(v) = long_value(arg, "--schedule", iter)? {
            schedule = Some(v);
        } else if let Some(v) = long_value(arg, "--replicas", iter)? {
            replicas = parse_i32("--replicas", &v)?;
        } else if let Some(v) = long_value(arg, "--port", iter)? {
            port = Some(parse_i32("--port", &v)?);
        } else if let Some(v) = long_value(arg, "--from-literal", iter)? {
            literals.push(key_value(&v, "literal")?);
        } else if let Some(v) = long_value(arg, "--from-file", iter)? {
            files.push(file_source(&v));
        } else if let Some(v) = long_value(arg, "--type", iter)? {
            secret_type = Some(v);
        } else if let Some(v) = long_value(arg, "--docker-server", iter)? {
            server = Some(v);
        } else if let Some(v) = long_value(arg, "--docker-username", iter)? {
            username = Some(v);
        } else if let Some(v) = long_value(arg, "--docker-password", iter)? {
            password = Some(v);
        } else if let Some(v) = long_value(arg, "--docker-email", iter)? {
            email = Some(v);
        } else {
            return Ok(false);
        }
        Ok(true)
    })?;
    // Flags that belong to another kind of object are refused rather than ignored.
    let allowed: &[&str] = match kind {
        "deployment" => &["image", "replicas", "port"],
        "job" => &["image"],
        "cronjob" => &["image", "schedule"],
        "configmap" => &["from-literal", "from-file"],
        "secret generic" => &["from-literal", "from-file", "type"],
        "secret docker-registry" => &["docker-server", "docker-username", "docker-password", "docker-email"],
        _ => &[],
    };
    let given = [
        ("image", image.is_some()),
        ("schedule", schedule.is_some()),
        ("replicas", replicas != 1),
        ("port", port.is_some()),
        ("from-literal", !literals.is_empty()),
        ("from-file", !files.is_empty()),
        ("type", secret_type.is_some()),
        ("docker-server", server.is_some()),
        ("docker-username", username.is_some()),
        ("docker-password", password.is_some()),
        ("docker-email", email.is_some()),
    ];
    if let Some((flag, _)) = given.iter().find(|(flag, set)| *set && !allowed.contains(flag)) {
        return Err(ParseError::UnknownFlag(format!("--{flag}")));
    }
    let name = match positional.as_slice() {
        [] => {
            return Err(ParseError::Builtin(
                "error: exactly NAME is required, got 0".to_string(),
            ))
        }
        [name] => name.to_string(),
        many => {
            return Err(ParseError::Builtin(format!(
                "error: exactly NAME is required, got {}",
                many.len()
            )))
        }
    };
    let object = match kind {
        "namespace" => CreateObject::Namespace,
        "deployment" => CreateObject::Deployment {
            image: image.ok_or(ParseError::MissingFlag("image"))?,
            replicas,
            port,
        },
        "job" => CreateObject::Job {
            image: image.ok_or(ParseError::MissingFlag("image"))?,
            command: trailing.iter().map(|w| w.to_string()).collect(),
        },
        "cronjob" => CreateObject::CronJob {
            image: image.ok_or(ParseError::MissingFlag("image"))?,
            schedule: schedule.ok_or(ParseError::MissingFlag("schedule"))?,
            command: trailing.iter().map(|w| w.to_string()).collect(),
        },
        "configmap" => CreateObject::ConfigMap { literals, files },
        "secret generic" => CreateObject::Secret {
            literals,
            files,
            secret_type,
        },
        _ => CreateObject::DockerRegistrySecret {
            server: server.unwrap_or_else(|| "https://index.docker.io/v1/".into()),
            username: username.ok_or(ParseError::MissingFlag("docker-username"))?,
            password: password.ok_or(ParseError::MissingFlag("docker-password"))?,
            email,
        },
    };
    Ok(Command::CreateObject(CreateArgs {
        name,
        namespace,
        object,
    }))
}

/// `TYPE NAME...` or `TYPE/NAME...` through the registry, limited to the kinds a command handles.
fn target(verb: &str, positional: &[&str], allowed: &[&str]) -> Result<(&'static str, Vec<String>), ParseError> {
    let mut kinds = KindArgs::default();
    for arg in positional {
        kinds.push(arg)?;
    }
    let (kind, names) = kinds.finish("act on")?;
    if !allowed.contains(&kind) {
        let plural = crate::api::info(kind).map_or_else(|| kind.to_string(), |i| i.resource());
        return Err(ParseError::Unsupported(format!("kubectl {verb} for {plural}")));
    }
    Ok((kind, names))
}

fn parse_set_image(args: &[&str]) -> Result<SetImageArgs, ParseError> {
    let (positional, _, namespace) = scan_args(args, |_, _| Ok(false))?;
    let (specs, resource): (Vec<&str>, Vec<&str>) = positional.iter().partition(|a| a.contains('='));
    let (kind, names) = target("set image", &resource, &["Deployment", "ReplicaSet", "Pod"])?;
    let name = match names.as_slice() {
        [] => return Err(ParseError::MissingName),
        [name] => name.clone(),
        [_, extra @ ..] => return Err(ParseError::UnexpectedArgs(extra.to_vec())),
    };
    if specs.is_empty() {
        return Err(ParseError::Builtin(
            "error: at least one image update is required".into(),
        ));
    }
    let images = specs
        .iter()
        .map(|s| key_value(s, "image spec"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SetImageArgs {
        kind,
        name,
        namespace,
        images,
    })
}

fn parse_scale(args: &[&str]) -> Result<ScaleArgs, ParseError> {
    let mut replicas = None;
    let (positional, _, namespace) = scan_args(args, |arg, iter| {
        match long_value(arg, "--replicas", iter)? {
            Some(v) => replicas = Some(parse_i32("--replicas", &v)?),
            None => return Ok(false),
        }
        Ok(true)
    })?;
    let (kind, names) = target("scale", &positional, &["Deployment", "ReplicaSet"])?;
    if names.is_empty() {
        return Err(ParseError::MissingName);
    }
    Ok(ScaleArgs {
        kind,
        names,
        namespace,
        replicas: replicas.ok_or(ParseError::MissingFlag("replicas"))?,
    })
}

fn parse_rollout(action: RolloutAction, args: &[&str]) -> Result<RolloutArgs, ParseError> {
    let mut to_revision = None;
    let undo = matches!(action, RolloutAction::Undo { .. });
    let (positional, _, namespace) = scan_args(args, |arg, iter| {
        if undo {
            if let Some(v) = long_value(arg, "--to-revision", iter)? {
                to_revision = Some(v.parse::<i64>().map_err(|_| ParseError::InvalidInt {
                    flag: "--to-revision",
                    value: v.clone(),
                })?);
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    let verb = match action {
        RolloutAction::Status => "rollout status",
        RolloutAction::History => "rollout history",
        RolloutAction::Undo { .. } => "rollout undo",
    };
    let (_, names) = target(verb, &positional, &["Deployment"])?;
    let name = match names.as_slice() {
        [] => return Err(ParseError::MissingName),
        [name] => name.clone(),
        [_, extra @ ..] => return Err(ParseError::UnexpectedArgs(extra.to_vec())),
    };
    let action = match action {
        RolloutAction::Undo { .. } => RolloutAction::Undo { to_revision },
        other => other,
    };
    Ok(RolloutArgs {
        action,
        name,
        namespace,
    })
}

fn parse_label(args: &[&str]) -> Result<LabelArgs, ParseError> {
    let mut overwrite = false;
    let (positional, _, namespace) = scan_args(args, |arg, _| {
        if arg == "--overwrite" {
            overwrite = true;
            return Ok(true);
        }
        // `foo-` removes a label; a bare `-` or `--x` is a flag-like word handled by the caller.
        Ok(false)
    })?;
    let (specs, resource): (Vec<&str>, Vec<&str>) = positional
        .iter()
        .partition(|a| a.contains('=') || (a.ends_with('-') && a.len() > 1));
    let (kind, names) = target(
        "label",
        &resource,
        &crate::api::kinds().iter().map(|k| k.kind).collect::<Vec<_>>(),
    )?;
    if names.is_empty() {
        return Err(ParseError::MissingName);
    }
    if specs.is_empty() {
        return Err(ParseError::Builtin(
            "error: at least one label update is required".into(),
        ));
    }
    let mut changes = Vec::new();
    for spec in specs {
        changes.push(match spec.strip_suffix('-').filter(|_| !spec.contains('=')) {
            Some(key) => LabelChange::Remove(key.to_string()),
            None => {
                let (key, value) = key_value(spec, "label spec")?;
                LabelChange::Set(key, value)
            }
        });
    }
    Ok(LabelArgs {
        kind,
        names,
        namespace,
        changes,
        overwrite,
    })
}

fn parse_expose(args: &[&str]) -> Result<ExposeArgs, ParseError> {
    let (mut port, mut target_port, mut service_name, mut service_type, mut protocol) = (None, None, None, None, None);
    let (positional, _, namespace) = scan_args(args, |arg, iter| {
        if let Some(v) = long_value(arg, "--port", iter)? {
            port = Some(parse_i32("--port", &v)?);
        } else if let Some(v) = long_value(arg, "--target-port", iter)? {
            target_port = Some(v);
        } else if let Some(v) = long_value(arg, "--name", iter)? {
            service_name = Some(v);
        } else if let Some(v) = long_value(arg, "--type", iter)? {
            service_type = Some(v);
        } else if let Some(v) = long_value(arg, "--protocol", iter)? {
            protocol = Some(v);
        } else {
            return Ok(false);
        }
        Ok(true)
    })?;
    let (kind, names) = target("expose", &positional, &["Pod", "Deployment", "ReplicaSet", "Service"])?;
    let name = match names.as_slice() {
        [] => return Err(ParseError::MissingName),
        [name] => name.clone(),
        [_, extra @ ..] => return Err(ParseError::UnexpectedArgs(extra.to_vec())),
    };
    if let Some(t) = service_type.as_deref() {
        if !["ClusterIP", "NodePort", "LoadBalancer"].contains(&t) {
            return Err(ParseError::Builtin(format!(
                "error: unknown service type {t:?} (one of ClusterIP, NodePort, LoadBalancer)"
            )));
        }
    }
    Ok(ExposeArgs {
        kind,
        name,
        namespace,
        port,
        target_port,
        service_name,
        service_type,
        protocol,
    })
}

/// `kubectl help [COMMAND...]`
fn parse_help_topic(args: &[&str]) -> Result<Command, ParseError> {
    let topic = args.join(" ");
    if crate::help::is_topic(&topic) {
        Ok(Command::Help(topic))
    } else {
        Err(ParseError::Builtin(format!("Unknown help topic [`{topic}`]")))
    }
}

/// `kubectl VERB ... --help`: the page of the command being asked about, found by taking as many
/// of the leading words as name a page (`create secret generic`).
fn help_for(verb: &str, rest: &[&str]) -> Result<Command, ParseError> {
    fn alias(word: &str) -> &str {
        match word {
            "ns" => "namespace",
            "deploy" => "deployment",
            "cm" => "configmap",
            other => other,
        }
    }
    let mut words = vec![verb.to_string()];
    words.extend(
        rest.iter()
            .filter(|a| !is_flag(a))
            .take(2)
            .map(|w| alias(w).to_string()),
    );
    for n in (1..=words.len()).rev() {
        let topic = words[..n].join(" ");
        if crate::help::is_topic(&topic) {
            return Ok(Command::Help(topic));
        }
    }
    Err(unknown_verb(verb))
}

fn parse_explain(args: &[&str]) -> Result<ExplainArgs, ParseError> {
    let mut targets: Vec<&str> = Vec::new();
    for arg in args {
        if is_flag(arg) {
            return Err(unknown_flag(arg));
        }
        targets.push(arg);
    }
    match targets.as_slice() {
        [] => Err(ParseError::MissingResource("explain")),
        [path] => Ok(ExplainArgs { path: path.to_string() }),
        [_, extra @ ..] => Err(ParseError::UnexpectedArgs(
            extra.iter().map(|a| a.to_string()).collect(),
        )),
    }
}

/// `cat` / `ls`: file operands only; options are refused the way coreutils refuses unknown ones.
fn parse_paths(command: &str, args: Vec<&str>) -> Result<Command, ParseError> {
    let mut paths = Vec::new();
    let mut operands_only = false;
    for arg in args {
        if !operands_only && arg == "--" {
            operands_only = true;
        } else if !operands_only && is_flag(arg) {
            let message = match arg.strip_prefix("--") {
                Some(long) => format!("{command}: unrecognized option '--{long}'"),
                None => format!("{command}: invalid option -- '{}'", arg.chars().nth(1).unwrap_or('-')),
            };
            return Err(ParseError::Builtin(message));
        } else {
            paths.push(arg.to_string());
        }
    }
    let args = PathArgs { paths };
    Ok(if command == "cat" {
        Command::Cat(args)
    } else {
        Command::Ls(args)
    })
}

fn parse_vim(command: &str, args: Vec<&str>) -> Result<Command, ParseError> {
    let mut paths = Vec::new();
    for arg in args {
        if is_flag(arg) && arg != "-" {
            return Err(ParseError::Builtin(format!(
                "{command}: unknown option '{arg}': options are not available in this editor"
            )));
        }
        paths.push(arg.to_string());
    }
    match paths.len() {
        0 => Err(ParseError::Builtin(format!(
            "{command}: no file given; usage: {command} FILE"
        ))),
        1 => Ok(Command::Vim(PathArgs { paths })),
        _ => Err(ParseError::Builtin(format!(
            "{command}: one file at a time; usage: {command} FILE"
        ))),
    }
}

const NAMESPACE_FLAG: &str = "-n, --namespace";

fn flag_value<'a>(iter: &mut impl Iterator<Item = &'a str>, flag: &'static str) -> Result<&'a str, ParseError> {
    iter.next().ok_or(ParseError::MissingValue(flag))
}

/// If `arg` is a namespace flag (`-n x`, `-nx`, `--namespace x`, `--namespace=x`), consume it
/// along with its value and return the namespace.
fn take_namespace<'a>(arg: &str, iter: &mut impl Iterator<Item = &'a str>) -> Result<Option<String>, ParseError> {
    if arg == "-n" || arg == "--namespace" {
        return Ok(Some(flag_value(iter, NAMESPACE_FLAG)?.to_string()));
    }
    if let Some(value) = arg.strip_prefix("--namespace=") {
        return Ok(Some(value.to_string()));
    }
    if !arg.starts_with("--") {
        if let Some(value) = arg.strip_prefix("-n") {
            return Ok(Some(value.to_string()));
        }
    }
    Ok(None)
}

/// Value of a `--long x` / `--long=x` flag, or `None` if `arg` is a different flag.
fn take_long<'a>(
    arg: &str,
    long: &'static str,
    iter: &mut impl Iterator<Item = &'a str>,
) -> Result<Option<String>, ParseError> {
    if arg == long {
        return Ok(Some(flag_value(iter, long)?.to_string()));
    }
    Ok(arg
        .strip_prefix(long)
        .and_then(|rest| rest.strip_prefix('='))
        .map(String::from))
}

/// Long flags real kubectl has (here or on other commands) that this terminal does not offer.
const UNSUPPORTED_LONG_FLAGS: &[&str] = &[
    "all",
    "all-containers",
    "allow-missing-template-keys",
    "as",
    "as-group",
    "cache-dir",
    "cascade",
    "certificate-authority",
    "chunk-size",
    "client-certificate",
    "client-key",
    "cluster",
    "context",
    "dry-run",
    "field-manager",
    "field-selector",
    "filename",
    "follow",
    "force-conflicts",
    "ignore-errors",
    "ignore-not-found",
    "insecure-skip-tls-verify",
    "kubeconfig",
    "kustomize",
    "label-columns",
    "limit-bytes",
    "match-server-version",
    "max-log-requests",
    "output",
    "overwrite",
    "password",
    "pod-running-timeout",
    "prefix",
    "profile",
    "prune",
    "raw",
    "recursive",
    "request-timeout",
    "save-config",
    "selector",
    "server",
    "server-side",
    "show-kind",
    "show-managed-fields",
    "since",
    "since-time",
    "sort-by",
    "subresource",
    "template",
    "timeout",
    "tls-server-name",
    "token",
    "user",
    "username",
    "v",
    "validate",
    "wait",
    "watch",
    "watch-only",
];
/// Short flags real kubectl has on commands where this terminal has none.
const UNSUPPORTED_SHORT_FLAGS: &str = "AefilLoRstvwkp";

/// A flag that did not match. If real kubectl has it, say it is not available here instead of
/// leaving the player to wonder whether they mistyped it.
fn unknown_flag(arg: &str) -> ParseError {
    let base = arg.split('=').next().unwrap_or(arg);
    let real = match base.strip_prefix("--") {
        Some(long) => UNSUPPORTED_LONG_FLAGS.contains(&long),
        None => base.chars().nth(1).is_some_and(|c| UNSUPPORTED_SHORT_FLAGS.contains(c)),
    };
    if real {
        ParseError::Unsupported(format!("the flag {base}"))
    } else {
        ParseError::UnknownFlag(arg.to_string())
    }
}

fn is_flag(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1
}

/// A resource argument that is not a kind this terminal knows: several at once, one real
/// Kubernetes has but this terminal does not simulate, or not a Kubernetes resource at all.
pub(crate) fn unsupported_resource(arg: &str) -> ParseError {
    if arg.contains(',') {
        ParseError::Unsupported(format!("listing several resource types at once ({arg})"))
    } else if crate::api::is_unsimulated_resource(arg) {
        ParseError::Unsupported(format!("the resource type {arg}"))
    } else {
        ParseError::UnsupportedResource(arg.to_string())
    }
}

/// Positional `TYPE`, `TYPE NAME...` or `TYPE/NAME...` arguments for the commands that act on
/// the original four kinds.
struct ResourceArgs {
    verb: &'static str,
    type_arg: Option<ResourceKind>,
    slash_kind: Option<ResourceKind>,
    names: Vec<String>,
}

impl ResourceArgs {
    fn for_verb(verb: &'static str) -> Self {
        ResourceArgs {
            verb,
            type_arg: None,
            slash_kind: None,
            names: Vec::new(),
        }
    }

    fn push(&mut self, arg: &str) -> Result<(), ParseError> {
        if let Some((kind, name)) = arg.split_once('/') {
            self.slash_kind = Some(ResourceKind::for_verb(self.verb, kind)?);
            self.names.push(name.to_string());
        } else if self.type_arg.is_none() {
            self.type_arg = Some(ResourceKind::for_verb(self.verb, arg)?);
        } else {
            self.names.push(arg.to_string());
        }
        Ok(())
    }

    fn finish(self, verb: &'static str) -> Result<(ResourceKind, Vec<String>), ParseError> {
        if self.type_arg.is_some() && self.slash_kind.is_some() {
            return Err(ParseError::MixedResourceForms);
        }
        let resource = self
            .type_arg
            .or(self.slash_kind)
            .ok_or(ParseError::MissingResource(verb))?;
        Ok((resource, self.names))
    }
}

/// `TYPE`, `TYPE NAME...` or `TYPE/NAME...` for `get`, resolved through the API registry so any
/// registered kind works.
#[derive(Default)]
struct KindArgs {
    type_arg: Option<&'static str>,
    slash_kind: Option<&'static str>,
    names: Vec<String>,
}

impl KindArgs {
    fn resolve(arg: &str) -> Result<&'static str, ParseError> {
        crate::api::kind_for_arg(arg)
            .map(|k| k.kind)
            .ok_or_else(|| unsupported_resource(arg))
    }

    fn push(&mut self, arg: &str) -> Result<(), ParseError> {
        if let Some((kind, name)) = arg.split_once('/') {
            self.slash_kind = Some(Self::resolve(kind)?);
            self.names.push(name.to_string());
        } else if self.type_arg.is_none() {
            self.type_arg = Some(Self::resolve(arg)?);
        } else {
            self.names.push(arg.to_string());
        }
        Ok(())
    }

    fn finish(self, verb: &'static str) -> Result<(&'static str, Vec<String>), ParseError> {
        if self.type_arg.is_some() && self.slash_kind.is_some() {
            return Err(ParseError::MixedResourceForms);
        }
        let kind = self
            .type_arg
            .or(self.slash_kind)
            .ok_or(ParseError::MissingResource(verb))?;
        Ok((kind, self.names))
    }
}

fn parse_get(args: &[&str]) -> Result<GetArgs, ParseError> {
    const OUTPUT_FLAG: &str = "--output";

    let mut positional = KindArgs::default();
    let mut namespace: Option<String> = None;
    let mut output = OutputFormat::Table;
    let mut selector = Selector::default();
    let (mut all_namespaces, mut no_headers, mut show_labels) = (false, false, false);

    let parse_selector = |text: &str| Selector::parse(text).map_err(ParseError::InvalidSelector);

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "-o" {
            output = OutputFormat::parse(flag_value(&mut iter, "-o, --output")?)?;
        } else if let Some(value) = take_long(arg, OUTPUT_FLAG, &mut iter)? {
            output = OutputFormat::parse(&value)?;
        } else if arg == "-l" {
            selector = parse_selector(flag_value(&mut iter, "-l, --selector")?)?;
        } else if let Some(value) = take_long(arg, "--selector", &mut iter)? {
            selector = parse_selector(&value)?;
        } else if arg == "-A" || arg == "--all-namespaces" {
            all_namespaces = true;
        } else if arg == "--no-headers" {
            no_headers = true;
        } else if arg == "--show-labels" {
            show_labels = true;
        } else if arg.starts_with("--") {
            return Err(unknown_flag(arg));
        } else if let Some(value) = arg.strip_prefix("-o") {
            output = OutputFormat::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("-l") {
            selector = parse_selector(value)?;
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg)?;
        }
    }

    let (kind, names) = positional.finish("get")?;
    if all_namespaces && !names.is_empty() {
        return Err(ParseError::NameAcrossNamespaces);
    }
    Ok(GetArgs {
        kind,
        names,
        namespace,
        output,
        selector,
        all_namespaces,
        no_headers,
        show_labels,
    })
}

/// The kinds `kubectl describe` can describe.
const DESCRIBABLE: &[&str] = &[
    "Pod",
    "Deployment",
    "ReplicaSet",
    "Event",
    "Namespace",
    "Service",
    "Job",
    "CronJob",
];

fn parse_describe(args: &[&str]) -> Result<DescribeArgs, ParseError> {
    let mut positional = KindArgs::default();
    let mut namespace: Option<String> = None;
    let mut selector = Selector::default();
    let parse_selector = |text: &str| Selector::parse(text).map_err(ParseError::InvalidSelector);

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "-l" {
            selector = parse_selector(flag_value(&mut iter, "-l, --selector")?)?;
        } else if let Some(value) = take_long(arg, "--selector", &mut iter)? {
            selector = parse_selector(&value)?;
        } else if let Some(value) = arg.strip_prefix("-l").filter(|_| !arg.starts_with("--")) {
            selector = parse_selector(value)?;
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg)?;
        }
    }

    let (kind, names) = positional.finish("describe")?;
    if !DESCRIBABLE.contains(&kind) {
        let plural = crate::api::info(kind).map_or_else(|| kind.to_string(), |i| i.resource());
        return Err(ParseError::Unsupported(format!("kubectl describe for {plural}")));
    }
    Ok(DescribeArgs {
        kind,
        names,
        namespace,
        selector,
    })
}

fn parse_logs(args: &[&str]) -> Result<LogsArgs, ParseError> {
    let mut pod: Option<String> = None;
    let mut namespace: Option<String> = None;
    let mut container: Option<String> = None;
    let mut previous = false;
    let mut tail: Option<usize> = None;
    let mut timestamps = false;

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "-p" || arg == "--previous" {
            previous = true;
        } else if arg == "--timestamps" {
            timestamps = true;
        } else if arg == "-c" {
            container = Some(flag_value(&mut iter, "-c, --container")?.to_string());
        } else if let Some(value) = take_long(arg, "--container", &mut iter)? {
            container = Some(value);
        } else if let Some(value) = take_long(arg, "--tail", &mut iter)? {
            tail = parse_tail(&value)?;
        } else if arg.starts_with("--") {
            return Err(unknown_flag(arg));
        } else if let Some(value) = arg.strip_prefix("-c") {
            container = Some(value.to_string());
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else if pod.is_none() {
            pod = Some(arg.to_string());
        } else {
            // kubectl treats a second positional as the container name.
            container = Some(arg.to_string());
        }
    }

    let target = pod.ok_or(ParseError::LogsMissingPod)?;
    let (resource, name) = match target.split_once('/') {
        Some((kind, name)) => match ResourceKind::for_verb("logs", kind)? {
            kind @ (ResourceKind::Pod | ResourceKind::Deployment | ResourceKind::Job) => (kind, name.to_string()),
            other => return Err(ParseError::LogsUnsupportedTarget(other.plural().to_string())),
        },
        None => (ResourceKind::Pod, target),
    };
    Ok(LogsArgs {
        resource,
        name,
        namespace,
        container,
        previous,
        tail,
        timestamps,
    })
}

fn parse_delete(args: &[&str]) -> Result<Command, ParseError> {
    let mut positional = KindArgs::default();
    let mut has_positional = false;
    let mut namespace: Option<String> = None;
    let mut immediate = false;
    let mut files: Vec<String> = Vec::new();

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if take_files(arg, &mut iter, &mut files)? {
        } else if arg == "--force" || arg == "--now" {
            immediate = true;
        } else if let Some(value) = take_long(arg, "--grace-period", &mut iter)? {
            let grace: i64 = value.parse().map_err(|_| ParseError::InvalidInt {
                flag: "--grace-period",
                value: value.clone(),
            })?;
            immediate |= grace == 0;
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg)?;
            has_positional = true;
        }
    }

    if !files.is_empty() {
        if has_positional {
            return Err(ParseError::FilesAndResources);
        }
        return Ok(Command::DeleteFiles(ManifestArgs {
            files,
            namespace,
            force: false,
        }));
    }
    let (kind, names) = positional.finish("delete")?;
    if names.is_empty() {
        return Err(ParseError::MissingName);
    }
    Ok(Command::Delete(DeleteArgs {
        kind,
        names,
        namespace,
        immediate,
    }))
}

/// If `arg` is a file flag (`-f x`, `-fx`, `--filename x`, `--filename=x`), consume it and add
/// its comma-separated paths to `files`. Kustomize directories and stdin are refused.
fn take_files<'a>(
    arg: &str,
    iter: &mut impl Iterator<Item = &'a str>,
    files: &mut Vec<String>,
) -> Result<bool, ParseError> {
    if arg == "-k" || arg.starts_with("--kustomize") || (arg.starts_with("-k") && !arg.starts_with("--")) {
        return Err(ParseError::Unsupported("kustomize (-k)".into()));
    }
    let value = if arg == "-f" {
        flag_value(iter, "-f, --filename")?.to_string()
    } else if let Some(value) = take_long(arg, "--filename", iter)? {
        value
    } else if let Some(value) = arg
        .strip_prefix("-f")
        .filter(|v| !v.is_empty() && !arg.starts_with("--"))
    {
        value.to_string()
    } else {
        return Ok(false);
    };
    for path in value.split(',').filter(|p| !p.is_empty()) {
        if path == "-" {
            return Err(ParseError::Unsupported("reading manifests from stdin (-f -)".into()));
        }
        files.push(path.to_string());
    }
    Ok(true)
}

/// `apply` / `create` / `replace`: `-f FILE...` and `-n` (and `--force` for `replace`).
fn parse_manifest_args(args: &[&str], replace: bool) -> Result<ManifestArgs, ParseError> {
    let mut namespace: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut unexpected: Vec<String> = Vec::new();
    let mut force = false;

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if take_files(arg, &mut iter, &mut files)? {
        } else if arg == "--force" {
            if !replace {
                return Err(ParseError::ForceWithoutReplace);
            }
            force = true;
        } else if replace && take_long(arg, "--grace-period", &mut iter)?.is_some() {
            // Accepted with --force; the simulated replace never waits anyway.
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            unexpected.push(arg.to_string());
        }
    }
    if !unexpected.is_empty() {
        return Err(ParseError::UnexpectedArgs(unexpected));
    }
    if files.is_empty() {
        return Err(ParseError::MissingFilename);
    }
    Ok(ManifestArgs {
        files,
        namespace,
        force,
    })
}

/// `patch TYPE NAME -p PATCH [--type strategic|merge|json]` (or `TYPE/NAME`).
fn parse_patch(args: &[&str]) -> Result<PatchArgs, ParseError> {
    let mut positional: Vec<&str> = Vec::new();
    let mut namespace: Option<String> = None;
    let mut patch: Option<String> = None;
    let mut patch_type = PatchType::Strategic;

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "-p" {
            patch = Some(flag_value(&mut iter, "-p, --patch")?.to_string());
        } else if let Some(value) = take_long(arg, "--patch", &mut iter)? {
            patch = Some(value);
        } else if let Some(value) = take_long(arg, "--type", &mut iter)? {
            patch_type = match value.as_str() {
                "strategic" => PatchType::Strategic,
                "merge" => PatchType::Merge,
                "json" => PatchType::Json,
                other => return Err(ParseError::InvalidPatchType(other.to_string())),
            };
        } else if arg.starts_with("--") {
            return Err(unknown_flag(arg));
        } else if let Some(value) = arg.strip_prefix("-p") {
            patch = Some(value.to_string());
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg);
        }
    }

    let (resource, name) = match positional.as_slice() {
        [target] => match target.split_once('/') {
            Some((resource, name)) => (resource.to_string(), name.to_string()),
            None => return Err(ParseError::MissingName),
        },
        [resource, name] => (resource.to_string(), name.to_string()),
        [] => return Err(ParseError::MissingResource("patch")),
        [_, _, extra @ ..] => {
            return Err(ParseError::UnexpectedArgs(
                extra.iter().map(|a| a.to_string()).collect(),
            ))
        }
    };
    if crate::api::kind_for_arg(&resource).is_none() {
        return Err(ParseError::UnsupportedResource(resource));
    }
    let patch = patch.ok_or(ParseError::MissingPatch)?;
    Ok(PatchArgs {
        resource,
        name,
        namespace,
        patch,
        patch_type,
    })
}

fn parse_rollout_restart(args: &[&str]) -> Result<RolloutRestartArgs, ParseError> {
    let mut positional = ResourceArgs::for_verb("rollout restart");
    let mut namespace: Option<String> = None;

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg)?;
        }
    }

    let (resource, names) = positional.finish("restart")?;
    if names.is_empty() {
        return Err(ParseError::MissingName);
    }
    Ok(RolloutRestartArgs {
        resource,
        names,
        namespace,
    })
}

/// `KEY=VALUE` sets, `KEY-` removes.
fn parse_env_change(spec: &str) -> Result<EnvChange, ParseError> {
    let valid_name = |n: &str| {
        let mut chars = n.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '.' | '-'))
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    if let Some((name, value)) = spec.split_once('=') {
        if valid_name(name) {
            return Ok(EnvChange::Set {
                name: name.to_string(),
                value: value.to_string(),
            });
        }
    } else if let Some(name) = spec.strip_suffix('-') {
        if valid_name(name) {
            return Ok(EnvChange::Unset(name.to_string()));
        }
    }
    Err(ParseError::InvalidEnvSpec(spec.to_string()))
}

fn looks_like_env_spec(arg: &str) -> bool {
    arg.contains('=') || arg.ends_with('-')
}

fn parse_set_env(args: &[&str]) -> Result<SetEnvArgs, ParseError> {
    let mut positional: Vec<&str> = Vec::new();
    let mut namespace: Option<String> = None;
    let mut container: Option<String> = None;
    let mut list = false;
    let mut changes: Vec<EnvChange> = Vec::new();

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "--list" {
            list = true;
        } else if arg == "-c" {
            container = Some(flag_value(&mut iter, "-c, --container")?.to_string());
        } else if let Some(value) = take_long(arg, "--container", &mut iter)? {
            container = Some(value);
        } else if arg == "-e" {
            changes.push(parse_env_change(flag_value(&mut iter, "-e, --env")?)?);
        } else if let Some(value) = take_long(arg, "--env", &mut iter)? {
            changes.push(parse_env_change(&value)?);
        } else if arg.starts_with("--") {
            return Err(unknown_flag(arg));
        } else if let Some(value) = arg.strip_prefix("-c").filter(|v| !v.is_empty()) {
            container = Some(value.to_string());
        } else if is_flag(arg) && !looks_like_env_spec(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg);
        }
    }

    // `TYPE/NAME ENV...` or `TYPE NAME ENV...`
    let (first, rest) = positional
        .split_first()
        .ok_or(ParseError::MissingResource("set env on"))?;
    let (resource, name, specs): (ResourceKind, String, &[&str]) = match first.split_once('/') {
        Some((kind, name)) => (ResourceKind::for_verb("set env", kind)?, name.to_string(), rest),
        None => {
            let resource = ResourceKind::for_verb("set env", first)?;
            match rest.split_first() {
                Some((name, specs)) if !name.contains('=') => (resource, name.to_string(), specs),
                _ => return Err(ParseError::MissingName),
            }
        }
    };
    for spec in specs {
        changes.push(parse_env_change(spec)?);
    }
    if changes.is_empty() && !list {
        return Err(ParseError::SetEnvNoChanges);
    }
    Ok(SetEnvArgs {
        resource,
        name,
        namespace,
        container,
        list,
        changes,
    })
}

/// `--tail -1` (the kubectl default) means everything.
fn parse_tail(value: &str) -> Result<Option<usize>, ParseError> {
    let n: i64 = value.parse().map_err(|_| ParseError::InvalidInt {
        flag: "--tail",
        value: value.to_string(),
    })?;
    Ok(usize::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(names: &[&str], namespace: Option<&str>, output: OutputFormat) -> Result<Command, ParseError> {
        Ok(Command::Get(GetArgs {
            kind: "Pod",
            names: names.iter().map(|s| s.to_string()).collect(),
            namespace: namespace.map(String::from),
            output,
            selector: Selector::default(),
            all_namespaces: false,
            no_headers: false,
            show_labels: false,
        }))
    }

    fn logs(pod: &str) -> LogsArgs {
        LogsArgs {
            resource: ResourceKind::Pod,
            name: pod.into(),
            namespace: None,
            container: None,
            previous: false,
            tail: None,
            timestamps: false,
        }
    }

    #[test]
    fn parses_resource_aliases() {
        for cmd in [
            "kubectl get pods",
            "kubectl get pod",
            "kubectl get po",
            "  kubectl   get   pods  ",
        ] {
            assert_eq!(parse(cmd), get(&[], None, OutputFormat::Table), "{cmd}");
        }
        for (cmd, kind) in [
            ("kubectl get events", "Event"),
            ("kubectl get ev", "Event"),
            ("kubectl get deployments", "Deployment"),
            ("kubectl get deploy", "Deployment"),
            ("kubectl get replicasets", "ReplicaSet"),
            ("kubectl get rs", "ReplicaSet"),
            ("kubectl get cm", "ConfigMap"),
            ("kubectl get ns", "Namespace"),
            ("kubectl get svc", "Service"),
            ("kubectl get nodes", "Node"),
            ("kubectl get secrets", "Secret"),
            ("kubectl get deployments.apps", "Deployment"),
        ] {
            let Ok(Command::Get(args)) = parse(cmd) else {
                panic!("{cmd}")
            };
            assert_eq!(args.kind, kind, "{cmd}");
        }
    }

    #[test]
    fn parses_get_selectors_scopes_and_jsonpath() {
        let Ok(Command::Get(args)) = parse("kubectl get pods -l app=web,tier!=db -A --no-headers") else {
            panic!("parses")
        };
        assert!(args.all_namespaces && args.no_headers);
        assert_eq!(args.selector, Selector::parse("app=web,tier!=db").unwrap());
        for cmd in [
            "kubectl get pods -lapp=web,tier!=db",
            "kubectl get pods --selector app=web,tier!=db",
        ] {
            let Ok(Command::Get(other)) = parse(cmd) else {
                panic!("{cmd}")
            };
            assert_eq!(other.selector, args.selector, "{cmd}");
        }
        let Ok(Command::Get(args)) = parse("kubectl get pod web -o jsonpath={.metadata.name}") else {
            panic!("parses")
        };
        assert_eq!(args.output, OutputFormat::JsonPath("{.metadata.name}".into()));
        assert_eq!(parse("kubectl get pods -o jsonpath"), Err(ParseError::MissingTemplate));
        assert_eq!(
            parse("kubectl get pods -o go-template={{.x}}"),
            Err(ParseError::UnknownOutputFormat("go-template".into()))
        );
        assert_eq!(parse("kubectl get pod web -A"), Err(ParseError::NameAcrossNamespaces));
        assert_eq!(
            parse("kubectl get pods -l 'a b'").unwrap_err().to_string(),
            "error: couldn't parse the selector string \"a b\": invalid selector"
        );
    }

    #[test]
    fn parses_namespace_flags() {
        for cmd in [
            "kubectl get pods -n kube-system",
            "kubectl get pods -nkube-system",
            "kubectl get pods --namespace kube-system",
            "kubectl get pods --namespace=kube-system",
            "kubectl get -n kube-system pods",
        ] {
            assert_eq!(parse(cmd), get(&[], Some("kube-system"), OutputFormat::Table), "{cmd}");
        }
    }

    #[test]
    fn parses_output_flags() {
        for (flag, format) in [
            ("wide", OutputFormat::Wide),
            ("json", OutputFormat::Json),
            ("yaml", OutputFormat::Yaml),
            ("name", OutputFormat::Name),
        ] {
            for cmd in [
                format!("kubectl get pods -o {flag}"),
                format!("kubectl get pods -o{flag}"),
                format!("kubectl get pods --output {flag}"),
                format!("kubectl get pods --output={flag}"),
            ] {
                assert_eq!(parse(&cmd), get(&[], None, format.clone()), "{cmd}");
            }
        }
    }

    #[test]
    fn parses_names() {
        assert_eq!(parse("kubectl get pod web"), get(&["web"], None, OutputFormat::Table));
        assert_eq!(
            parse("kubectl get pods a b"),
            get(&["a", "b"], None, OutputFormat::Table)
        );
        assert_eq!(
            parse("kubectl get pod/a pod/b"),
            get(&["a", "b"], None, OutputFormat::Table)
        );
        assert_eq!(
            parse("kubectl get po/a -n x -o yaml"),
            get(&["a"], Some("x"), OutputFormat::Yaml)
        );
    }

    #[test]
    fn rejects_mixed_resource_forms() {
        assert_eq!(parse("kubectl get pods pod/a"), Err(ParseError::MixedResourceForms));
    }

    #[test]
    fn parses_describe() {
        let describe = |names: &[&str], ns: Option<&str>| {
            Ok(Command::Describe(DescribeArgs {
                kind: "Pod",
                names: names.iter().map(|s| s.to_string()).collect(),
                namespace: ns.map(String::from),
                selector: Selector::default(),
            }))
        };
        assert_eq!(parse("kubectl describe pod web"), describe(&["web"], None));
        assert_eq!(parse("kubectl describe pod/web -n x"), describe(&["web"], Some("x")));
        assert_eq!(parse("kubectl describe pods"), describe(&[], None));
        assert_eq!(parse("kubectl describe"), Err(ParseError::MissingResource("describe")));
        assert_eq!(
            parse("kubectl describe pod web -o yaml"),
            Err(ParseError::Unsupported("the flag -o".into()))
        );
    }

    #[test]
    fn parses_logs() {
        assert_eq!(parse("kubectl logs web"), Ok(Command::Logs(logs("web"))));
        assert_eq!(parse("kubectl logs pod/web"), Ok(Command::Logs(logs("web"))));
        assert_eq!(
            parse("kubectl logs web -c app -p --tail 5 --timestamps -n x"),
            Ok(Command::Logs(LogsArgs {
                resource: ResourceKind::Pod,
                name: "web".into(),
                namespace: Some("x".into()),
                container: Some("app".into()),
                previous: true,
                tail: Some(5),
                timestamps: true,
            }))
        );
        for cmd in [
            "kubectl logs web --container=app",
            "kubectl logs web --container app",
            "kubectl logs web -capp",
            "kubectl logs web app",
        ] {
            let Ok(Command::Logs(args)) = parse(cmd) else {
                panic!("{cmd}")
            };
            assert_eq!(args.container.as_deref(), Some("app"), "{cmd}");
        }
        assert_eq!(
            parse("kubectl logs --previous web").map(|c| matches!(c, Command::Logs(a) if a.previous)),
            Ok(true)
        );
        let Ok(Command::Logs(args)) = parse("kubectl logs deploy/api") else {
            panic!()
        };
        assert_eq!((args.resource, args.name.as_str()), (ResourceKind::Deployment, "api"));
        let Ok(Command::Logs(args)) = parse("kubectl logs web --tail=-1") else {
            panic!()
        };
        assert_eq!(args.tail, None);
        let Ok(Command::Logs(args)) = parse("kubectl logs web --tail=0") else {
            panic!()
        };
        assert_eq!(args.tail, Some(0));
    }

    #[test]
    fn logs_errors() {
        assert_eq!(parse("kubectl logs"), Err(ParseError::LogsMissingPod));
        assert_eq!(
            parse("kubectl logs web --tail x"),
            Err(ParseError::InvalidInt {
                flag: "--tail",
                value: "x".into()
            })
        );
        assert_eq!(
            parse("kubectl logs web --tail"),
            Err(ParseError::MissingValue("--tail"))
        );
        assert_eq!(
            parse("kubectl logs web -f"),
            Err(ParseError::Unsupported("the flag -f".into()))
        );
        assert_eq!(parse("kubectl logs web -z"), Err(ParseError::UnknownFlag("-z".into())));
        assert_eq!(
            parse("kubectl logs event/x"),
            Err(ParseError::LogsUnsupportedTarget("events".into()))
        );
        assert_eq!(
            parse("kubectl logs rs/x"),
            Err(ParseError::LogsUnsupportedTarget("replicasets.apps".into()))
        );
        assert_eq!(
            parse("kubectl logs nodes/x"),
            Err(ParseError::Unsupported("kubectl logs for nodes".into()))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("rm x"), Err(ParseError::UnknownCommand("rm".into())));
        assert_eq!(
            parse("ls -l"),
            Err(ParseError::Builtin("ls: invalid option -- 'l'".into()))
        );
        assert_eq!(
            parse("cat --number x"),
            Err(ParseError::Builtin("cat: unrecognized option '--number'".into()))
        );
        assert_eq!(
            parse("cat -- -x a.yaml"),
            Ok(Command::Cat(PathArgs {
                paths: vec!["-x".into(), "a.yaml".into()]
            }))
        );
        assert_eq!(parse("ls"), Ok(Command::Ls(PathArgs { paths: vec![] })));
        assert_eq!(parse("kubectl"), Err(ParseError::MissingVerb));
        assert_eq!(parse("kubectl nope"), Err(ParseError::UnknownVerb("nope".into())));
        assert_eq!(parse("kubectl get"), Err(ParseError::MissingResource("get")));
        assert_eq!(
            parse("kubectl get widgets"),
            Err(ParseError::UnsupportedResource("widgets".into()))
        );
        assert_eq!(
            parse("kubectl get pods -n"),
            Err(ParseError::MissingValue("-n, --namespace"))
        );
        assert_eq!(
            parse("kubectl get pods -o"),
            Err(ParseError::MissingValue("-o, --output"))
        );
        assert_eq!(parse("kubectl get pods -x"), Err(ParseError::UnknownFlag("-x".into())));
        assert_eq!(
            parse("kubectl get pods --nope"),
            Err(ParseError::UnknownFlag("--nope".into()))
        );
        assert_eq!(
            parse("kubectl get pods -o xml"),
            Err(ParseError::UnknownOutputFormat("xml".into()))
        );
    }

    #[test]
    fn error_messages_use_kubectl_wording() {
        assert_eq!(
            ParseError::UnknownFlag("--nope".into()).to_string(),
            "error: unknown flag: --nope"
        );
        assert_eq!(
            ParseError::UnknownFlag("-x".into()).to_string(),
            "error: unknown shorthand flag: 'x' in -x"
        );
        assert!(ParseError::MissingResource("describe")
            .to_string()
            .contains("type of resource to describe"));
        assert!(ParseError::LogsMissingPod
            .to_string()
            .starts_with("error: expected 'logs [-f] [-p]"));
    }

    #[test]
    fn shell_split_handles_quotes_and_escapes() {
        let words = |s: &str| shell_split(s).unwrap();
        assert_eq!(words("  a  b\tc "), ["a", "b", "c"]);
        assert_eq!(words(r#"set env d K="a b" 'c d'"#), ["set", "env", "d", "K=a b", "c d"]);
        assert_eq!(words(r#"x "" y"#), ["x", "", "y"]);
        assert_eq!(words(r#"a\ b "c\"d""#), ["a b", "c\"d"]);
        assert_eq!(shell_split("it's"), Err(ParseError::UnterminatedQuote));
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(parse("kubectl get pods \"x"), Err(ParseError::UnterminatedQuote));
        assert_eq!(
            ParseError::UnterminatedQuote.to_string(),
            "error: unterminated quoted string"
        );
    }

    fn set_env(resource: ResourceKind, name: &str, changes: Vec<EnvChange>) -> SetEnvArgs {
        SetEnvArgs {
            resource,
            name: name.into(),
            namespace: None,
            container: None,
            list: false,
            changes,
        }
    }

    #[test]
    fn parses_set_env() {
        let set = |n: &str, v: &str| EnvChange::Set {
            name: n.into(),
            value: v.into(),
        };
        let expected = Ok(Command::SetEnv(set_env(
            ResourceKind::Deployment,
            "api",
            vec![
                set("DATABASE_URL", "pg://db?a=b"),
                EnvChange::Unset("DATABSE_URL".into()),
            ],
        )));
        for cmd in [
            "kubectl set env deployment/api DATABASE_URL=pg://db?a=b DATABSE_URL-",
            "kubectl set env deploy api DATABASE_URL=pg://db?a=b DATABSE_URL-",
            "kubectl set env deployment api DATABASE_URL='pg://db?a=b' DATABSE_URL-",
            "kubectl set env deploy/api -e DATABASE_URL=pg://db?a=b --env=DATABSE_URL-",
        ] {
            assert_eq!(parse(cmd), expected, "{cmd}");
        }
        let Ok(Command::SetEnv(args)) = parse(r#"kubectl set env deploy/api MSG="hello world" -c app -n prod"#) else {
            panic!()
        };
        assert_eq!(args.changes, [set("MSG", "hello world")]);
        assert_eq!(
            (args.container.as_deref(), args.namespace.as_deref()),
            (Some("app"), Some("prod"))
        );
        let Ok(Command::SetEnv(args)) = parse("kubectl set env deploy/api --list") else {
            panic!()
        };
        assert!(args.list && args.changes.is_empty());
        let Ok(Command::SetEnv(args)) = parse("kubectl set env deploy/api EMPTY=") else {
            panic!()
        };
        assert_eq!(args.changes, [set("EMPTY", "")]);
    }

    #[test]
    fn set_env_errors() {
        assert_eq!(parse("kubectl set env deploy/api"), Err(ParseError::SetEnvNoChanges));
        assert_eq!(parse("kubectl set env deploy A=B"), Err(ParseError::MissingName));
        assert_eq!(parse("kubectl set env"), Err(ParseError::MissingResource("set env on")));
        assert_eq!(
            parse("kubectl set env deploy/api 9bad=x"),
            Err(ParseError::InvalidEnvSpec("9bad=x".into()))
        );
        assert_eq!(
            parse("kubectl set env deploy/api junk"),
            Err(ParseError::InvalidEnvSpec("junk".into()))
        );
        assert_eq!(
            parse("kubectl set selector deploy/api x=y"),
            Err(ParseError::Unsupported("kubectl set selector".into()))
        );
        assert_eq!(parse("kubectl set"), Err(ParseError::MissingSubcommand("set")));
        assert_eq!(
            ParseError::UnknownSubcommand {
                parent: "set",
                name: "image".into()
            }
            .to_string(),
            "error: unknown command \"image\" for \"kubectl set\""
        );
    }

    #[test]
    fn parses_rollout_restart_and_delete() {
        assert_eq!(
            parse("kubectl rollout restart deployment/api -n x"),
            Ok(Command::RolloutRestart(RolloutRestartArgs {
                resource: ResourceKind::Deployment,
                names: vec!["api".into()],
                namespace: Some("x".into())
            }))
        );
        assert_eq!(parse("kubectl rollout restart deploy"), Err(ParseError::MissingName));
        assert_eq!(
            parse("kubectl rollout pause deploy/api"),
            Err(ParseError::Unsupported("kubectl rollout pause".into()))
        );
        assert_eq!(parse("kubectl rollout"), Err(ParseError::MissingSubcommand("rollout")));

        let delete = |names: &[&str], immediate: bool| {
            Ok(Command::Delete(DeleteArgs {
                kind: "Pod",
                names: names.iter().map(|s| s.to_string()).collect(),
                namespace: None,
                immediate,
            }))
        };
        assert_eq!(parse("kubectl delete pod a b"), delete(&["a", "b"], false));
        assert_eq!(parse("kubectl delete pod/a"), delete(&["a"], false));
        assert_eq!(parse("kubectl delete pod a --force"), delete(&["a"], true));
        assert_eq!(parse("kubectl delete pod a --grace-period=0"), delete(&["a"], true));
        assert_eq!(parse("kubectl delete pod a --grace-period 30"), delete(&["a"], false));
        assert_eq!(parse("kubectl delete pod a --now"), delete(&["a"], true));
        assert_eq!(parse("kubectl delete pod"), Err(ParseError::MissingName));
        assert_eq!(parse("kubectl delete"), Err(ParseError::MissingResource("delete")));
    }
}
