//! kubectl command-line parsing. Pure Rust with no wasm-bindgen types.

use std::fmt;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Wide,
    Json,
    Yaml,
    Name,
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, ParseError> {
        match value {
            "" => Ok(OutputFormat::Table),
            "wide" => Ok(OutputFormat::Wide),
            "json" => Ok(OutputFormat::Json),
            "yaml" => Ok(OutputFormat::Yaml),
            "name" => Ok(OutputFormat::Name),
            other => Err(ParseError::UnknownOutputFormat(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetArgs {
    pub resource: ResourceKind,
    pub names: Vec<String>,
    pub namespace: Option<String>,
    pub output: OutputFormat,
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

/// `-f` files for `apply`, `create` and `delete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestArgs {
    pub files: Vec<String>,
    pub namespace: Option<String>,
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
                "error: unable to match a printer suitable for the output format \"{format}\", allowed formats are: json,name,wide,yaml"
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
        Some(other) => return Err(ParseError::UnknownCommand(other.to_string())),
    }

    let verb = tokens.next().ok_or(ParseError::MissingVerb)?;
    let rest: Vec<&str> = tokens.collect();

    match verb {
        "get" => parse_get(&rest).map(Command::Get),
        "describe" => parse_describe(&rest).map(Command::Describe),
        "logs" => parse_logs(&rest).map(Command::Logs),
        "delete" => parse_delete(&rest),
        "apply" => parse_manifest_args(&rest).map(Command::Apply),
        "create" => match rest.first() {
            Some(sub) if !is_flag(sub) => Err(ParseError::UnknownSubcommand {
                parent: "create",
                name: sub.to_string(),
            }),
            _ => parse_manifest_args(&rest).map(Command::Create),
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

fn parse_get(args: &[&str]) -> Result<GetArgs, ParseError> {
    const OUTPUT_FLAG: &str = "--output";

    let mut positional = ResourceArgs::default();
    let mut namespace: Option<String> = None;
    let mut output = OutputFormat::Table;

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if arg == "-o" {
            output = OutputFormat::parse(flag_value(&mut iter, "-o, --output")?)?;
        } else if let Some(value) = take_long(arg, OUTPUT_FLAG, &mut iter)? {
            output = OutputFormat::parse(&value)?;
        } else if arg.starts_with("--") {
            return Err(unknown_flag(arg));
        } else if let Some(value) = arg.strip_prefix("-o") {
            output = OutputFormat::parse(value)?;
        } else if is_flag(arg) {
            return Err(unknown_flag(arg));
        } else {
            positional.push(arg)?;
        }
    }

    let (resource, names) = positional.finish("get")?;
    Ok(GetArgs {
        resource,
        names,
        namespace,
        output,
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
        return Ok(Command::DeleteFiles(ManifestArgs { files, namespace }));
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

/// `apply` / `create`: `-f FILE...` and `-n`.
fn parse_manifest_args(args: &[&str]) -> Result<ManifestArgs, ParseError> {
    let mut namespace: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut unexpected: Vec<String> = Vec::new();

    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(ns) = take_namespace(arg, &mut iter)? {
            namespace = Some(ns);
        } else if take_files(arg, &mut iter, &mut files)? {
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
    Ok(ManifestArgs { files, namespace })
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
            resource: ResourceKind::Pod,
            names: names.iter().map(|s| s.to_string()).collect(),
            namespace: namespace.map(String::from),
            output,
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
            ("kubectl get events", ResourceKind::Event),
            ("kubectl get ev", ResourceKind::Event),
            ("kubectl get deployments", ResourceKind::Deployment),
            ("kubectl get deploy", ResourceKind::Deployment),
            ("kubectl get replicasets", ResourceKind::ReplicaSet),
            ("kubectl get rs", ResourceKind::ReplicaSet),
        ] {
            let Ok(Command::Get(args)) = parse(cmd) else {
                panic!("{cmd}")
            };
            assert_eq!(args.resource, kind, "{cmd}");
        }
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
                assert_eq!(parse(&cmd), get(&[], None, format), "{cmd}");
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
        assert_eq!(parse("ls"), Err(ParseError::UnknownCommand("ls".into())));
        assert_eq!(parse("kubectl"), Err(ParseError::MissingVerb));
        assert_eq!(parse("kubectl nope"), Err(ParseError::UnknownVerb("nope".into())));
        assert_eq!(parse("kubectl get"), Err(ParseError::MissingResource("get")));
        assert_eq!(
            parse("kubectl get nodes"),
            Err(ParseError::UnsupportedResource("nodes".into()))
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
