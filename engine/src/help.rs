//! `--help` for every command this terminal has, built from one table. Tab completion reads its
//! flags from the same table, and a test checks that the parser accepts every documented flag,
//! so help never lists something that does not work (SPEC V7).

/// One option of a command.
pub struct Flag {
    pub short: Option<&'static str>,
    pub long: &'static str,
    /// What kubectl shows as the default (`''`, `false`, `-1`); empty for none.
    pub default: &'static str,
    /// Whether the flag takes a value.
    pub value: bool,
    pub help: &'static str,
}

const fn flag(
    short: Option<&'static str>,
    long: &'static str,
    default: &'static str,
    value: bool,
    help: &'static str,
) -> Flag {
    Flag {
        short,
        long,
        default,
        value,
        help,
    }
}

const NAMESPACE: Flag = flag(
    Some("-n"),
    "--namespace",
    "''",
    true,
    "If present, the namespace scope for this CLI request",
);
const FILENAME: Flag = flag(
    Some("-f"),
    "--filename",
    "[]",
    true,
    "Filename to use for the resource (a file in this terminal; use cat and echo > to make one)",
);

/// A kubectl command.
pub struct Verb {
    /// `get`, `set env`, `rollout restart`.
    pub name: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
    /// `(comment, command)` pairs.
    pub examples: &'static [(&'static str, &'static str)],
    pub flags: &'static [Flag],
    /// Arguments that make a valid command line to try the flags on (for tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub sample: &'static str,
}

pub const VERBS: &[Verb] = &[
    Verb {
        name: "get",
        summary: "Display one or many resources",
        description: "Prints a table of the most important information about the specified resources.",
        usage: "kubectl get (TYPE [NAME ...] | TYPE/NAME ...) [flags] [options]",
        examples: &[
            ("List all pods in the current namespace", "kubectl get pods"),
            ("List pods in every namespace, with more detail", "kubectl get pods -A -o wide"),
            ("List pods that carry a label", "kubectl get pods -l app=web"),
            ("Print one field of an object", "kubectl get pod web -o jsonpath='{.status.podIP}'"),
            ("Print an object as YAML", "kubectl get deployment web -o yaml"),
        ],
        flags: &[
            flag(Some("-A"), "--all-namespaces", "false", false, "If present, list the requested object(s) across all namespaces"),
            flag(None, "--no-headers", "false", false, "Do not print the table's header line"),
            NAMESPACE,
            flag(Some("-o"), "--output", "''", true, "Output format. One of: json, jsonpath=TEMPLATE, name, wide, yaml"),
            flag(
                Some("-l"),
                "--selector",
                "''",
                true,
                "Selector (label query) to filter on: =, ==, !=, in, notin, a bare key, !key, joined with commas",
            ),
        ],
        sample: "pods",
    },
    Verb {
        name: "describe",
        summary: "Show details of a specific resource or group of resources",
        description: "Print a detailed description of the selected resources, including related resources such as events.",
        usage: "kubectl describe (TYPE NAME | TYPE/NAME) [options]",
        examples: &[
            ("Describe a pod", "kubectl describe pod web-7d9f8b6c5-x2k4p"),
            ("Describe a deployment", "kubectl describe deployment/web"),
        ],
        flags: &[NAMESPACE],
        sample: "pod x",
    },
    Verb {
        name: "logs",
        summary: "Print the logs for a container in a pod",
        description: "Print the logs for a container in a pod, or for a pod of a deployment.",
        usage: "kubectl logs (POD | TYPE/NAME) [-c CONTAINER] [options]",
        examples: &[
            ("Logs of a pod", "kubectl logs web-7d9f8b6c5-x2k4p"),
            ("Logs of one of a deployment's pods", "kubectl logs deploy/payments-api"),
            ("The previous run of a container that restarted", "kubectl logs -p payments-api-5c7d4f9b8-9hjv2"),
            ("The last 20 lines, with timestamps", "kubectl logs --tail=20 --timestamps web-7d9f8b6c5-x2k4p"),
        ],
        flags: &[
            flag(Some("-c"), "--container", "''", true, "Print the logs of this container"),
            NAMESPACE,
            flag(Some("-p"), "--previous", "false", false, "Print the logs for the previous instance of the container in a pod if it exists"),
            flag(None, "--tail", "-1", true, "Lines of recent log file to display. Defaults to -1, showing all log lines"),
            flag(None, "--timestamps", "false", false, "Include timestamps on each line in the log output"),
        ],
        sample: "x",
    },
    Verb {
        name: "set env",
        summary: "Update environment variables on a pod template",
        description: "List environment variable definitions in a deployment's pod template, or add, change and remove them.\n\nNAME=value sets a variable; NAME- removes it.",
        usage: "kubectl set env (TYPE NAME | TYPE/NAME) KEY_1=VAL_1 ... KEY_N=VAL_N [options]",
        examples: &[
            ("Update a deployment's environment", "kubectl set env deployment/web STORAGE_DIR=/local"),
            ("Remove a variable", "kubectl set env deployment/web STORAGE_DIR-"),
            ("List the environment", "kubectl set env deployment/web --list"),
        ],
        flags: &[
            flag(Some("-c"), "--container", "'*'", true, "The name of a container in the pod template to apply the change to"),
            flag(Some("-e"), "--env", "[]", true, "Specify a key-value pair for an environment variable to set"),
            flag(None, "--list", "false", false, "If true, display the environment and any changes in the standard format"),
            NAMESPACE,
        ],
        sample: "deployment/x A=b",
    },
    Verb {
        name: "rollout restart",
        summary: "Restart a resource",
        description: "Restart a deployment: its pods are replaced by new ones, a few at a time.",
        usage: "kubectl rollout restart (TYPE NAME | TYPE/NAME) [options]",
        examples: &[("Restart a deployment", "kubectl rollout restart deployment/web")],
        flags: &[NAMESPACE],
        sample: "deployment/x",
    },
    Verb {
        name: "delete",
        summary: "Delete resources by file names, or by resources and names",
        description: "Delete resources by file name, or by type and name.\n\nPods are given a grace period to stop; --force or --grace-period=0 skips it.",
        usage: "kubectl delete ((-f FILENAME) | (TYPE NAME ...)) [options]",
        examples: &[
            ("Delete a pod", "kubectl delete pod web-7d9f8b6c5-x2k4p"),
            ("Delete the objects in a file", "kubectl delete -f web.yaml"),
            ("Delete a pod without waiting", "kubectl delete pod web-7d9f8b6c5-x2k4p --force"),
        ],
        flags: &[
            FILENAME,
            flag(None, "--force", "false", false, "If true, immediately remove resources from API and bypass graceful deletion"),
            flag(None, "--grace-period", "-1", true, "Period of time in seconds given to the resource to terminate gracefully; 0 means immediately"),
            NAMESPACE,
            flag(None, "--now", "false", false, "If true, resources are signaled for immediate shutdown (same as --grace-period=0)"),
        ],
        sample: "pod x",
    },
    Verb {
        name: "apply",
        summary: "Apply a configuration to a resource by file name",
        description: "Apply a configuration to a resource by file name. The objects are created if they do not exist, and merged into the live ones if they do.",
        usage: "kubectl apply -f FILENAME [options]",
        examples: &[
            ("Apply the configuration in web.yaml", "kubectl apply -f web.yaml"),
            ("Apply into a namespace", "kubectl apply -n homicide -f web.yaml"),
        ],
        flags: &[FILENAME, NAMESPACE],
        sample: "-f a.yaml",
    },
    Verb {
        name: "create",
        summary: "Create a resource from a file",
        description: "Create a resource from a file. It fails if the object already exists.",
        usage: "kubectl create -f FILENAME [options]",
        examples: &[("Create the objects in web.yaml", "kubectl create -f web.yaml")],
        flags: &[FILENAME, NAMESPACE],
        sample: "-f a.yaml",
    },
    Verb {
        name: "replace",
        summary: "Replace a resource by file name",
        description: "Replace a resource by file name. The file's object takes the place of the live one as a whole.\n\nPod specs are mostly immutable: replace --force deletes the object and creates it again.",
        usage: "kubectl replace -f FILENAME [options]",
        examples: &[
            ("Replace a pod using the data in pod.yaml", "kubectl replace -f pod.yaml"),
            ("Force replace: delete and re-create the resource", "kubectl replace --force -f pod.yaml"),
        ],
        flags: &[
            FILENAME,
            flag(None, "--force", "false", false, "If true, immediately remove resources from API and bypass graceful deletion, then re-create them"),
            flag(None, "--grace-period", "-1", true, "Period of time in seconds given to the resource to terminate gracefully"),
            NAMESPACE,
        ],
        sample: "-f a.yaml",
    },
    Verb {
        name: "patch",
        summary: "Update fields of a resource",
        description: "Update fields of a resource using a strategic merge patch, a JSON merge patch, or a JSON patch.",
        usage: "kubectl patch (TYPE NAME | TYPE/NAME) -p PATCH [options]",
        examples: &[
            ("Scale a deployment", "kubectl patch deployment web -p '{\"spec\":{\"replicas\":3}}'"),
            ("Add a toleration to a pod with a JSON patch", "kubectl patch pod web --type json -p '[{\"op\":\"add\",\"path\":\"/spec/tolerations\",\"value\":[{\"operator\":\"Exists\"}]}]'"),
        ],
        flags: &[
            NAMESPACE,
            flag(Some("-p"), "--patch", "''", true, "The patch to be applied to the resource, as JSON or YAML"),
            flag(None, "--type", "'strategic'", true, "The type of patch being provided; one of [json merge strategic]"),
        ],
        sample: "pod x -p {}",
    },
    Verb {
        name: "explain",
        summary: "Get documentation for a resource",
        description: "Describe the fields of a resource, as documented by the Kubernetes API. Name a field by its path to look inside it.",
        usage: "kubectl explain RESOURCE[.FIELD ...] [options]",
        examples: &[
            ("Get the documentation of a pod", "kubectl explain pods"),
            ("Get the documentation of a nested field", "kubectl explain pods.spec.containers"),
        ],
        flags: &[],
        sample: "pods",
    },
];

/// `kubectl --help`: the commands, grouped like kubectl groups them.
const GROUPS: &[(&str, &[&str])] = &[
    ("Basic Commands", &["create", "explain", "get", "delete", "set"]),
    ("Deploy Commands", &["rollout"]),
    ("Troubleshooting and Debugging Commands", &["describe", "logs"]),
    ("Advanced Commands", &["apply", "patch", "replace"]),
];

const PARENTS: &[(&str, &str, &[&str])] = &[
    ("set", "Set specific features on objects", &["env"]),
    ("rollout", "Manage the rollout of a resource", &["restart"]),
];

fn verb(name: &str) -> Option<&'static Verb> {
    VERBS.iter().find(|v| v.name == name)
}

/// The flags of a command (`get`, `set`, `rollout`), as the words typed.
pub fn flags_of(command: &str) -> Vec<&'static str> {
    let name = match command {
        "set" => "set env",
        "rollout" => "rollout restart",
        other => other,
    };
    verb(name)
        .map(|v| {
            v.flags
                .iter()
                .flat_map(|f| f.short.into_iter().chain([f.long]))
                .collect()
        })
        .unwrap_or_default()
}

fn summary_of(command: &str) -> &'static str {
    PARENTS
        .iter()
        .find(|(name, _, _)| *name == command)
        .map(|(_, summary, _)| *summary)
        .or_else(|| verb(command).map(|v| v.summary))
        .unwrap_or_default()
}

fn overview() -> String {
    let mut out =
        String::from("kubectl controls the Kubernetes cluster manager, as far as this terminal simulates it.\n\n");
    for (title, commands) in GROUPS {
        out.push_str(&format!("{title}:\n"));
        for command in *commands {
            out.push_str(&format!("  {command:<10}{}\n", summary_of(command)));
        }
        out.push('\n');
    }
    out.push_str("Usage:\n  kubectl [command] [flags] [options]\n\n");
    out.push_str("Use \"kubectl <command> --help\" for more information about a given command.");
    out
}

fn parent(name: &str, summary: &str, subcommands: &[&str]) -> String {
    let mut out = format!("{summary}.\n\nAvailable Commands:\n");
    for sub in subcommands {
        let full = format!("{name} {sub}");
        out.push_str(&format!("  {sub:<10}{}\n", summary_of(&full)));
    }
    out.push_str(&format!(
        "\nUsage:\n  kubectl {name} SUBCOMMAND [options]\n\nUse \"kubectl {name} <command> --help\" for more information about a given command."
    ));
    out
}

fn render_flag(f: &Flag) -> String {
    let names = match f.short {
        Some(short) => format!("{short}, {}", f.long),
        None => format!("    {}", f.long),
    };
    let default = if f.default.is_empty() {
        String::new()
    } else {
        format!("={}", f.default)
    };
    format!("    {names}{default}:\n\t{}", f.help)
}

fn render_verb(v: &Verb) -> String {
    let mut out = format!("{}.\n\n{}\n\n", v.summary, v.description);
    out.push_str("Examples:\n");
    for (comment, command) in v.examples {
        out.push_str(&format!("  # {comment}\n  {command}\n\n"));
    }
    if !v.flags.is_empty() {
        out.push_str("Options:\n");
        for f in v.flags {
            out.push_str(&render_flag(f));
            out.push_str("\n\n");
        }
    }
    out.push_str(&format!(
        "Usage:\n  {} [options]",
        v.usage.trim_end_matches(" [options]")
    ));
    out
}

/// The help for a topic: `""` for `kubectl --help`, else a command (`get`, `set`, `set env`).
pub fn kubectl_help(topic: &str) -> String {
    if topic.is_empty() {
        return overview();
    }
    if let Some((name, summary, subs)) = PARENTS.iter().find(|(name, _, _)| *name == topic) {
        return parent(name, summary, subs);
    }
    match verb(topic) {
        Some(v) => render_verb(v),
        None => overview(),
    }
}

/// Whether `topic` is something `kubectl_help` has a page for.
pub fn is_topic(topic: &str) -> bool {
    topic.is_empty() || verb(topic).is_some() || PARENTS.iter().any(|(name, _, _)| *name == topic)
}

// ---- shell commands -------------------------------------------------------------------------

/// A shell command of the terminal and what it understands.
pub struct ShellCommand {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub flags: &'static [Flag],
    /// Arguments to run the flags against (for tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub sample: &'static str,
}

pub const SHELL: &[ShellCommand] = &[
    ShellCommand {
        name: "cat",
        usage: "cat FILE...",
        summary: "Print files to the terminal. Without files, in a pipeline, copy the input.",
        flags: &[],
        sample: "a.txt",
    },
    ShellCommand {
        name: "ls",
        usage: "ls [FILE]...",
        summary: "List the files and directories of the terminal.",
        flags: &[],
        sample: "",
    },
    ShellCommand {
        name: "base64",
        usage: "base64 [OPTION]... [FILE]",
        summary: "Base64 encode or decode FILE, or standard input, to standard output.",
        flags: &[
            flag(Some("-d"), "--decode", "", false, "decode data"),
            flag(
                Some("-w"),
                "--wrap",
                "",
                true,
                "wrap encoded lines after COLS characters (default 76); 0 disables wrapping",
            ),
        ],
        sample: "a.txt",
    },
    ShellCommand {
        name: "grep",
        usage: "grep [OPTION]... PATTERNS [FILE]...",
        summary: "Search for PATTERNS in each FILE, or standard input. PATTERNS are basic regular expressions.",
        flags: &[
            flag(
                Some("-E"),
                "--extended-regexp",
                "",
                false,
                "PATTERNS are extended regular expressions",
            ),
            flag(Some("-F"), "--fixed-strings", "", false, "PATTERNS are strings"),
            flag(Some("-e"), "--regexp", "", true, "use PATTERNS for matching"),
            flag(Some("-i"), "--ignore-case", "", false, "ignore case distinctions"),
            flag(Some("-v"), "--invert-match", "", false, "select non-matching lines"),
            flag(
                Some("-c"),
                "--count",
                "",
                false,
                "print only a count of selected lines per FILE",
            ),
            flag(
                Some("-n"),
                "--line-number",
                "",
                false,
                "print line number with output lines",
            ),
        ],
        sample: "x a.txt",
    },
    ShellCommand {
        name: "head",
        usage: "head [OPTION]... [FILE]...",
        summary: "Print the first 10 lines of each FILE, or standard input.",
        flags: &[flag(
            Some("-n"),
            "--lines",
            "",
            true,
            "print the first NUM lines instead of the first 10",
        )],
        sample: "a.txt",
    },
];

/// `COMMAND --help` for the terminal's shell commands (`echo` has none: like the shell's own, it
/// prints what it is given).
pub fn shell_help(name: &str) -> Option<String> {
    let command = SHELL.iter().find(|c| c.name == name)?;
    let mut out = format!("Usage: {}\n{}\n", command.usage, command.summary);
    if !command.flags.is_empty() {
        out.push('\n');
    }
    for f in command.flags {
        let names = match f.short {
            Some(short) => format!("{short}, {}", f.long),
            None => format!("    {}", f.long),
        };
        let value = if f.value { "=VALUE" } else { "" };
        out.push_str(&format!("  {names}{value}\n\t{}\n", f.help));
    }
    out.push_str("      --help\n\tdisplay this help and exit");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse, Command, ParseError};

    #[test]
    fn every_documented_flag_is_accepted_by_the_parser() {
        for v in VERBS {
            for f in v.flags {
                for name in f.short.into_iter().chain([f.long]) {
                    let value = if f.value { " x" } else { "" };
                    let line = format!("kubectl {} {} {name}{value}", v.name, v.sample)
                        .replace("kubectl set env ", "kubectl set env ");
                    match parse(&line) {
                        Err(ParseError::UnknownFlag(flag)) => {
                            panic!("`{line}`: help documents {flag}, the parser refuses it")
                        }
                        Err(ParseError::UnknownSubcommand { .. }) | Err(ParseError::UnknownVerb(_)) => {
                            panic!("`{line}` is not a command")
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn every_verb_has_a_page_reachable_by_its_help_flag() {
        for v in VERBS {
            let line = format!("kubectl {} --help", v.name);
            assert_eq!(parse(&line), Ok(Command::Help(v.name.into())), "{line}");
            let line = format!("kubectl {} -h", v.name);
            assert_eq!(parse(&line), Ok(Command::Help(v.name.into())), "{line}");
            let page = kubectl_help(v.name);
            assert!(page.starts_with(&format!("{}.\n", v.summary)), "{page}");
            assert!(
                page.contains(&format!("Usage:\n  {}", v.usage.trim_end_matches(" [options]"))),
                "{page}"
            );
            for (_, example) in v.examples {
                assert!(example.starts_with("kubectl "), "{example}");
            }
        }
    }

    #[test]
    fn every_command_in_the_overview_has_a_page_and_every_page_is_listed() {
        let listed: Vec<&str> = GROUPS.iter().flat_map(|(_, cs)| cs.iter().copied()).collect();
        for command in &listed {
            assert!(is_topic(command), "{command}");
            assert!(overview().contains(&format!("  {command:<10}")), "{command}");
        }
        for v in VERBS {
            let top = v.name.split(' ').next().unwrap();
            assert!(listed.contains(&top), "{} is not in the overview", v.name);
        }
        for (name, _, subs) in PARENTS {
            for sub in *subs {
                assert!(parent(name, "", subs).contains(&format!("  {sub:<10}")));
                assert!(verb(&format!("{name} {sub}")).is_some());
            }
        }
    }

    #[test]
    fn pages_look_like_kubectls() {
        let page = kubectl_help("get");
        assert!(
            page.starts_with("Display one or many resources.\n\nPrints a table"),
            "{page}"
        );
        assert!(
            page.contains("Examples:\n  # List all pods in the current namespace\n  kubectl get pods\n"),
            "{page}"
        );
        assert!(page.contains("Options:\n    -A, --all-namespaces=false:\n\tIf present, list the requested object(s) across all namespaces\n"), "{page}");
        assert!(
            page.contains("    -n, --namespace='':\n\tIf present, the namespace scope"),
            "{page}"
        );
        assert!(
            page.contains("        --no-headers=false:") || page.contains("    --no-headers=false:"),
            "{page}"
        );
        assert!(
            page.ends_with("Usage:\n  kubectl get (TYPE [NAME ...] | TYPE/NAME ...) [flags] [options]"),
            "{page}"
        );
        assert!(kubectl_help("explain").ends_with("Usage:\n  kubectl explain RESOURCE[.FIELD ...] [options]"));
        assert!(!kubectl_help("explain").contains("Options:"), "explain has no options");
    }

    #[test]
    fn parent_commands_list_their_subcommands() {
        assert_eq!(
            kubectl_help("set"),
            "Set specific features on objects.\n\nAvailable Commands:\n  env       Update environment variables on a pod template\n\nUsage:\n  kubectl set SUBCOMMAND [options]\n\nUse \"kubectl set <command> --help\" for more information about a given command."
        );
        assert!(kubectl_help("rollout").contains("  restart   Restart a resource"));
        assert_eq!(parse("kubectl set --help"), Ok(Command::Help("set".into())));
        assert_eq!(
            parse("kubectl rollout restart -h"),
            Ok(Command::Help("rollout restart".into()))
        );
    }

    #[test]
    fn the_overview_lists_only_what_works() {
        let page = kubectl_help("");
        for word in [
            "get", "describe", "logs", "apply", "patch", "replace", "explain", "rollout", "set", "create", "delete",
        ] {
            assert!(page.contains(&format!("  {word} ")), "{word}");
        }
        for word in ["exec", "scale", "expose", "edit", "run", "top", "drain"] {
            assert!(!page.contains(&format!("  {word} ")), "{word} is not supported yet");
        }
        assert_eq!(parse("kubectl --help"), Ok(Command::Help(String::new())));
        assert_eq!(parse("kubectl -h"), Ok(Command::Help(String::new())));
        assert_eq!(parse("kubectl help"), Ok(Command::Help(String::new())));
        assert_eq!(parse("kubectl help get"), Ok(Command::Help("get".into())));
    }

    #[test]
    fn flags_for_completion_come_from_the_table() {
        assert_eq!(
            flags_of("get"),
            [
                "-A",
                "--all-namespaces",
                "--no-headers",
                "-n",
                "--namespace",
                "-o",
                "--output",
                "-l",
                "--selector"
            ]
        );
        assert_eq!(flags_of("set"), flags_of("set env"));
        assert_eq!(flags_of("rollout"), ["-n", "--namespace"]);
        assert!(flags_of("explain").is_empty() && flags_of("nope").is_empty());
    }

    #[test]
    fn shell_commands_document_only_what_they_accept() {
        use crate::manifests::Files;
        for c in SHELL {
            assert!(shell_help(c.name)
                .unwrap()
                .starts_with(&format!("Usage: {}\n", c.usage)));
            for f in c.flags {
                for name in f.short.into_iter().chain([f.long]) {
                    let value = if f.value { "=1" } else { "" };
                    let sep = if name.starts_with("--") { value } else { "" };
                    let args = if f.value && !name.starts_with("--") {
                        format!("{name} 1")
                    } else {
                        format!("{name}{sep}")
                    };
                    let words: Vec<String> = format!("{args} {}", c.sample)
                        .split_whitespace()
                        .map(String::from)
                        .collect();
                    let files = Files::from([("a.txt".to_string(), "x\n".to_string())]);
                    let out = crate::shell::try_filter(c.name, &words, "x\n", &files).expect("a filter");
                    let errors = out.stderr.join("\n");
                    assert!(
                        !errors.contains("invalid option") && !errors.contains("unrecognized option"),
                        "{} {args}: {errors}",
                        c.name
                    );
                }
            }
        }
        assert!(shell_help("echo").is_none() && shell_help("nope").is_none());
    }
}
