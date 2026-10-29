//! kubectl command-line parsing. Pure Rust with no wasm-bindgen types.

use std::fmt;

use crate::selector::Selector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Pod,
    Event,
    Deployment,
    ReplicaSet,
}

impl ResourceKind {
    pub(crate) fn from_arg(arg: &str) -> Result<Self, ParseError> {
        match arg {
            "pods" | "pod" | "po" => Ok(ResourceKind::Pod),
            "events" | "event" | "ev" => Ok(ResourceKind::Event),
            "deployments" | "deployment" | "deploy" => Ok(ResourceKind::Deployment),
            "replicasets" | "replicaset" | "rs" => Ok(ResourceKind::ReplicaSet),
            other => Err(ParseError::UnsupportedResource(other.to_string())),
        }
    }

    /// Kind as stored in the cluster (`Pod`).
    pub fn kind(self) -> &'static str {
        match self {
            ResourceKind::Pod => "Pod",
            ResourceKind::Event => "Event",
            ResourceKind::Deployment => "Deployment",
            ResourceKind::ReplicaSet => "ReplicaSet",
        }
    }

    /// Resource name used in kubectl messages (`pods`, `deployments.apps`).
    pub fn plural(self) -> &'static str {
        match self {
            ResourceKind::Pod => "pods",
            ResourceKind::Event => "events",
            ResourceKind::Deployment => "deployments.apps",
            ResourceKind::ReplicaSet => "replicasets.apps",
        }
    }

    /// Prefix kubectl uses in `-o name` output (`pod/web`, `deployment.apps/web`).
    pub fn name_prefix(self) -> &'static str {
        match self {
            ResourceKind::Pod => "pod",
            ResourceKind::Event => "event",
            ResourceKind::Deployment => "deployment.apps",
            ResourceKind::ReplicaSet => "replicaset.apps",
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribeArgs {
    pub resource: ResourceKind,
    pub names: Vec<String>,
    pub namespace: Option<String>,
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
    pub resource: ResourceKind,
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
    Unsupported(&'static str),
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
            ParseError::ForceWithoutReplace => write!(f, "error: unknown flag: --force"),
        }
    }
}

/// Split a command line like a POSIX shell: whitespace separates words, single and double
/// quotes group (and are removed), a backslash escapes the next character.
pub(crate) fn shell_split(input: &str) -> Result<Vec<String>, ParseError> {
    let mut words = Vec::new();
    let mut current = String::new();
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
                        words.push(std::mem::take(&mut current));
                        in_word = false;
                    }
                }
                c => {
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
        words.push(current);
    }
    Ok(words)
}

pub fn parse(input: &str) -> Result<Command, ParseError> {
    let words = shell_split(input)?;
    let mut tokens = words.iter().map(String::as_str);

    match tokens.next() {
        None => return Err(ParseError::Empty),
        Some("kubectl") => {}
        Some(builtin @ ("cat" | "ls")) => return parse_paths(builtin, tokens.collect()),
        Some(other) => return Err(ParseError::UnknownCommand(other.to_string())),
    }

    let verb = tokens.next().ok_or(ParseError::MissingVerb)?;
    let rest: Vec<&str> = tokens.collect();

    match verb {
        "get" => parse_get(&rest).map(Command::Get),
        "describe" => parse_describe(&rest).map(Command::Describe),
        "logs" => parse_logs(&rest).map(Command::Logs),
        "delete" => parse_delete(&rest),
        "apply" => parse_manifest_args(&rest, false).map(Command::Apply),
        "replace" => parse_manifest_args(&rest, true).map(Command::Replace),
        "patch" => parse_patch(&rest).map(Command::Patch),
        "create" => match rest.first() {
            Some(sub) if !is_flag(sub) => Err(ParseError::UnknownSubcommand {
                parent: "create",
                name: sub.to_string(),
            }),
            _ => parse_manifest_args(&rest, false).map(Command::Create),
        },
        "set" => match rest.split_first() {
            Some((&"env", args)) => parse_set_env(args).map(Command::SetEnv),
            Some((other, _)) => Err(ParseError::UnknownSubcommand {
                parent: "set",
                name: other.to_string(),
            }),
            None => Err(ParseError::MissingSubcommand("set")),
        },
        "rollout" => match rest.split_first() {
            Some((&"restart", args)) => parse_rollout_restart(args).map(Command::RolloutRestart),
            Some((other, _)) => Err(ParseError::UnknownSubcommand {
                parent: "rollout",
                name: other.to_string(),
            }),
            None => Err(ParseError::MissingSubcommand("rollout")),
        },
        other => Err(ParseError::UnknownVerb(other.to_string())),
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

fn unknown_flag(arg: &str) -> ParseError {
    ParseError::UnknownFlag(arg.to_string())
}

fn is_flag(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1
}

/// Positional `TYPE`, `TYPE NAME...` or `TYPE/NAME...` arguments shared by `get` and `describe`.
#[derive(Default)]
struct ResourceArgs {
    type_arg: Option<ResourceKind>,
    slash_kind: Option<ResourceKind>,
    names: Vec<String>,
}

impl ResourceArgs {
    fn push(&mut self, arg: &str) -> Result<(), ParseError> {
        if let Some((kind, name)) = arg.split_once('/') {
            self.slash_kind = Some(ResourceKind::from_arg(kind)?);
            self.names.push(name.to_string());
        } else if self.type_arg.is_none() {
            self.type_arg = Some(ResourceKind::from_arg(arg)?);
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
            .ok_or_else(|| ParseError::UnsupportedResource(arg.to_string()))
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
    let (mut all_namespaces, mut no_headers) = (false, false);

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
    })
}

fn parse_describe(args: &[&str]) -> Result<DescribeArgs, ParseError> {
    let mut positional = ResourceArgs::default();
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

    let (resource, names) = positional.finish("describe")?;
    Ok(DescribeArgs {
        resource,
        names,
        namespace,
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
        Some((kind, name)) => match ResourceKind::from_arg(kind)? {
            kind @ (ResourceKind::Pod | ResourceKind::Deployment) => (kind, name.to_string()),
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
    let mut positional = ResourceArgs::default();
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
    let (resource, names) = positional.finish("delete")?;
    if names.is_empty() {
        return Err(ParseError::MissingName);
    }
    Ok(Command::Delete(DeleteArgs {
        resource,
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
        return Err(ParseError::Unsupported("kustomize (-k)"));
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
            return Err(ParseError::Unsupported("reading manifests from stdin (-f -)"));
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
    let mut positional = ResourceArgs::default();
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
        Some((kind, name)) => (ResourceKind::from_arg(kind)?, name.to_string(), rest),
        None => {
            let resource = ResourceKind::from_arg(first)?;
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
                resource: ResourceKind::Pod,
                names: names.iter().map(|s| s.to_string()).collect(),
                namespace: ns.map(String::from),
            }))
        };
        assert_eq!(parse("kubectl describe pod web"), describe(&["web"], None));
        assert_eq!(parse("kubectl describe pod/web -n x"), describe(&["web"], Some("x")));
        assert_eq!(parse("kubectl describe pods"), describe(&[], None));
        assert_eq!(parse("kubectl describe"), Err(ParseError::MissingResource("describe")));
        assert_eq!(
            parse("kubectl describe pod web -o yaml"),
            Err(ParseError::UnknownFlag("-o".into()))
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
        assert_eq!(parse("kubectl logs web -f"), Err(ParseError::UnknownFlag("-f".into())));
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
            Err(ParseError::UnsupportedResource("nodes".into()))
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
            parse("kubectl set image deploy/api x=y"),
            Err(ParseError::UnknownSubcommand {
                parent: "set",
                name: "image".into()
            })
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
            parse("kubectl rollout status deploy/api"),
            Err(ParseError::UnknownSubcommand {
                parent: "rollout",
                name: "status".into()
            })
        );
        assert_eq!(parse("kubectl rollout"), Err(ParseError::MissingSubcommand("rollout")));

        let delete = |names: &[&str], immediate: bool| {
            Ok(Command::Delete(DeleteArgs {
                resource: ResourceKind::Pod,
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
