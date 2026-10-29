//! Tab completion for the terminal. Lives in the engine so the frontend never needs to know
//! which verbs, flags or resources exist: it sends the text left of the cursor and gets back the
//! words that could complete the word being typed.

use serde::Serialize;

use crate::parser::{shell_split, ResourceKind};
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::{ClusterStore, StoreError};

const VERBS: [&str; 6] = ["delete", "describe", "get", "logs", "rollout", "set"];
const OUTPUT_FORMATS: [&str; 4] = ["json", "name", "wide", "yaml"];
/// Plural names offered for resource types; aliases are the fallback when nothing matches.
const TYPES: [&str; 4] = ["deployments", "events", "pods", "replicasets"];
const TYPE_ALIASES: [&str; 8] = ["deploy", "deployment", "ev", "event", "po", "pod", "replicaset", "rs"];
/// Flags that consume the following word.
const VALUE_FLAGS: [&str; 10] = [
    "-n",
    "--namespace",
    "-o",
    "--output",
    "-c",
    "--container",
    "-e",
    "--env",
    "--grace-period",
    "--tail",
];

/// What could complete the word under the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Completion {
    /// Character offset in the line where the word being completed starts.
    pub start: usize,
    /// Full replacement words (not suffixes), sorted and de-duplicated.
    pub candidates: Vec<String>,
}

/// Complete the last word of `line` (the text left of the cursor).
pub fn complete(line: &str, store: &ClusterStore) -> Completion {
    let split = line
        .rfind(char::is_whitespace)
        .map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let (before, word) = line.split_at(split);
    let start = before.chars().count();
    let none = Completion {
        start,
        candidates: Vec::new(),
    };

    // Quoted or escaped words are left to the user.
    if word.contains(['\'', '"', '\\']) {
        return none;
    }
    let Ok(previous) = shell_split(before) else { return none };
    let Ok(mut candidates) = candidates(&previous, word, store) else {
        return none;
    };
    candidates.retain(|c| c.starts_with(word));
    candidates.sort();
    candidates.dedup();
    Completion { start, candidates }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn candidates(previous: &[String], word: &str, store: &ClusterStore) -> Result<Vec<String>, StoreError> {
    let position = previous.len();
    if position == 0 {
        return Ok(strings(&["kubectl"]));
    }
    if previous[0] != "kubectl" {
        return Ok(Vec::new());
    }
    if position == 1 {
        return Ok(strings(&VERBS));
    }

    // Where this verb's own arguments begin.
    let verb = previous[1].as_str();
    let args_from = match verb {
        "set" | "rollout" => {
            let sub = if verb == "set" { "env" } else { "restart" };
            if position == 2 {
                return Ok(strings(&[sub]));
            }
            if previous[2] != sub {
                return Ok(Vec::new());
            }
            3
        }
        _ => 2,
    };
    let args = &previous[args_from..];
    let namespace = namespace_of(args);

    // A flag's value.
    if let Some(flag) = args.last().filter(|a| VALUE_FLAGS.contains(&a.as_str())) {
        return flag_values(flag, verb, args, &namespace, store);
    }
    // `--flag=value`
    if let Some((flag, _)) = word.split_once('=').filter(|(f, _)| f.starts_with("--")) {
        let values = flag_values(flag, verb, args, &namespace, store)?;
        return Ok(values.into_iter().map(|v| format!("{flag}={v}")).collect());
    }
    if word.starts_with('-') {
        return Ok(strings(flags_for(verb)));
    }

    // Positional arguments (flags and their values removed).
    let positional = positionals(args);
    let names_in = |kind: ResourceKind| -> Result<Vec<String>, StoreError> { names(store, kind, &namespace) };

    if verb == "logs" {
        return match positional.len() {
            0 => {
                if let Some((kind, _)) = word.split_once('/') {
                    return slash_names(kind, &namespace, store);
                }
                let mut all = names_in(ResourceKind::Pod)?;
                all.extend(
                    names_in(ResourceKind::Deployment)?
                        .into_iter()
                        .map(|n| format!("deploy/{n}")),
                );
                Ok(all)
            }
            1 => containers_of(store, &namespace, positional[0]),
            _ => Ok(Vec::new()),
        };
    }

    // get / describe / delete / rollout restart / set env: TYPE [NAME...] or TYPE/NAME...
    if let Some((kind, _)) = word.split_once('/') {
        return slash_names(kind, &namespace, store);
    }
    let Some(first) = positional.first().copied() else {
        return Ok(resource_types(word));
    };
    if first.contains('/') {
        return Ok(resource_types(word));
    }
    let Ok(kind) = ResourceKind::from_arg(first) else {
        return Ok(Vec::new());
    };
    if verb == "set" && positional.len() >= 2 {
        return Ok(Vec::new()); // environment assignments are free text
    }
    names_in(kind)
}

fn resource_types(word: &str) -> Vec<String> {
    let plain: Vec<String> = TYPES
        .iter()
        .filter(|t| t.starts_with(word))
        .map(|t| t.to_string())
        .collect();
    if plain.is_empty() {
        TYPE_ALIASES.iter().map(|a| a.to_string()).collect()
    } else {
        plain
    }
}

/// `TYPE/` + names of that type.
fn slash_names(kind: &str, namespace: &str, store: &ClusterStore) -> Result<Vec<String>, StoreError> {
    let Ok(resource) = ResourceKind::from_arg(kind) else {
        return Ok(Vec::new());
    };
    Ok(names(store, resource, namespace)?
        .into_iter()
        .map(|n| format!("{kind}/{n}"))
        .collect())
}

fn names(store: &ClusterStore, kind: ResourceKind, namespace: &str) -> Result<Vec<String>, StoreError> {
    Ok(store
        .list(kind.kind(), namespace)?
        .iter()
        .filter_map(|m| m["metadata"]["name"].as_str().map(String::from))
        .collect())
}

fn flags_for(verb: &str) -> &'static [&'static str] {
    match verb {
        "get" => &["-n", "--namespace", "-o", "--output"],
        "describe" => &["-n", "--namespace"],
        "logs" => &[
            "-n",
            "--namespace",
            "-c",
            "--container",
            "-p",
            "--previous",
            "--tail",
            "--timestamps",
        ],
        "set" => &["-n", "--namespace", "-c", "--container", "-e", "--env", "--list"],
        "delete" => &["-n", "--namespace", "--force", "--grace-period", "--now"],
        "rollout" => &["-n", "--namespace"],
        _ => &[],
    }
}

/// The namespace selected earlier on the line, else the default.
fn namespace_of(args: &[String]) -> String {
    let mut namespace = DEFAULT_NAMESPACE.to_string();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "-n" || arg == "--namespace" {
            if let Some(value) = iter.next() {
                namespace = value.clone();
            }
        } else if let Some(value) = arg.strip_prefix("--namespace=") {
            namespace = value.to_string();
        } else if let Some(value) = arg
            .strip_prefix("-n")
            .filter(|v| !v.is_empty() && !arg.starts_with("--"))
        {
            namespace = value.to_string();
        }
    }
    namespace
}

/// Arguments that are neither flags nor flag values.
fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if VALUE_FLAGS.contains(&arg.as_str()) {
            iter.next();
        } else if !arg.starts_with('-') {
            out.push(arg.as_str());
        }
    }
    out
}

fn flag_values(
    flag: &str,
    verb: &str,
    args: &[String],
    namespace: &str,
    store: &ClusterStore,
) -> Result<Vec<String>, StoreError> {
    match flag {
        "-n" | "--namespace" => store.namespaces(),
        "-o" | "--output" if verb == "get" => Ok(strings(&OUTPUT_FORMATS)),
        "-c" | "--container" => {
            let target = positionals(args);
            // `logs POD`, `set env TYPE NAME` / `set env TYPE/NAME`
            let reference = match (verb, target.as_slice()) {
                ("logs", [pod, ..]) => Some((*pod).to_string()),
                (_, [first, ..]) if first.contains('/') => Some((*first).to_string()),
                (_, [kind, name, ..]) => Some(format!("{kind}/{name}")),
                _ => None,
            };
            match reference {
                Some(r) => containers_of(store, namespace, &r),
                None => Ok(Vec::new()),
            }
        }
        _ => Ok(Vec::new()),
    }
}

/// Container names of `POD`, `pod/NAME` or `deploy/NAME`.
fn containers_of(store: &ClusterStore, namespace: &str, reference: &str) -> Result<Vec<String>, StoreError> {
    let (kind, name) = match reference.split_once('/') {
        Some((kind, name)) => match ResourceKind::from_arg(kind) {
            Ok(k) => (k, name),
            Err(_) => return Ok(Vec::new()),
        },
        None => (ResourceKind::Pod, reference),
    };
    let Some(manifest) = store.get(kind.kind(), namespace, name)? else {
        return Ok(Vec::new());
    };
    let containers = match kind {
        ResourceKind::Pod => &manifest["spec"]["containers"],
        ResourceKind::Deployment | ResourceKind::ReplicaSet => &manifest["spec"]["template"]["spec"]["containers"],
        ResourceKind::Event => return Ok(Vec::new()),
    };
    Ok(containers
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["name"].as_str().map(String::from))
        .collect())
}

#[cfg(test)]
mod tests {
    use crate::sim::Engine;

    fn engine() -> Engine {
        Engine::from_scenario(
            include_str!("../../scenarios/crashloop-env-typo/scenario.json"),
            crate::apps::AppSet::bundled(),
        )
        .unwrap()
    }

    /// `(start, candidates)` for a line.
    fn complete(line: &str) -> (usize, Vec<String>) {
        let c = engine().complete(line);
        (c.start, c.candidates)
    }

    fn words(c: &[String]) -> Vec<&str> {
        c.iter().map(String::as_str).collect()
    }

    #[test]
    fn first_word_and_verbs() {
        assert_eq!(complete(""), (0, vec!["kubectl".to_string()]));
        assert_eq!(complete("kub").1, ["kubectl"]);
        assert_eq!(complete("ls").1, Vec::<String>::new());
        let (start, verbs) = complete("kubectl ");
        assert_eq!(start, 8);
        assert_eq!(words(&verbs), ["delete", "describe", "get", "logs", "rollout", "set"]);
        assert_eq!(complete("kubectl de").1, ["delete", "describe"]);
        assert_eq!(complete("kubectl lo").1, ["logs"]);
    }

    #[test]
    fn subcommands() {
        assert_eq!(complete("kubectl set ").1, ["env"]);
        assert_eq!(complete("kubectl rollout r").1, ["restart"]);
        assert_eq!(complete("kubectl set image ").1, Vec::<String>::new());
    }

    #[test]
    fn resource_types_with_alias_fallback() {
        assert_eq!(
            complete("kubectl get ").1,
            ["deployments", "events", "pods", "replicasets"]
        );
        assert_eq!(complete("kubectl get p").1, ["pods"]);
        assert_eq!(complete("kubectl get r").1, ["replicasets"]);
        assert_eq!(complete("kubectl describe rs").1, ["rs"]); // no plural matches "rs": aliases
        assert_eq!(complete("kubectl delete dep").1, ["deployments"]);
        assert_eq!(
            complete("kubectl rollout restart ").1,
            ["deployments", "events", "pods", "replicasets"]
        );
    }

    #[test]
    fn object_names_come_from_the_live_cluster() {
        let (start, names) = complete("kubectl get pods pay");
        assert_eq!(start, 17);
        assert_eq!(names.len(), 1);
        assert!(names[0].starts_with("payments-api-"), "{names:?}");
        assert_eq!(complete("kubectl get pods web").1.len(), 2);
        assert_eq!(complete("kubectl get deploy ").1, ["payments-api", "web"]);
        assert_eq!(complete("kubectl describe deployment pay").1, ["payments-api"]);
        assert_eq!(complete("kubectl get pods nomatch").1, Vec::<String>::new());
        assert_eq!(complete("kubectl get nodes ").1, Vec::<String>::new());
    }

    #[test]
    fn slash_forms_keep_the_type_prefix() {
        let (start, names) = complete("kubectl get pod/pay");
        assert_eq!(start, 12);
        assert!(
            names.len() == 1 && names[0].starts_with("pod/payments-api-"),
            "{names:?}"
        );
        assert_eq!(complete("kubectl rollout restart deploy/w").1, ["deploy/web"]);
        assert_eq!(complete("kubectl get nodes/x").1, Vec::<String>::new());
    }

    #[test]
    fn namespace_flag_scopes_names_and_completes_its_own_value() {
        let (_, system) = complete("kubectl get pods -n kube-system ");
        assert_eq!(words(&system), ["coredns-6d8c4cb4d-5fwtn", "etcd-control-plane"]);
        assert_eq!(
            complete("kubectl get pods --namespace=kube-system e").1,
            ["etcd-control-plane"]
        );
        assert_eq!(complete("kubectl get pods -n ").1, ["default", "kube-system"]);
        assert_eq!(complete("kubectl get pods -n kube").1, ["kube-system"]);
        assert_eq!(
            complete("kubectl get pods --namespace=kube").1,
            ["--namespace=kube-system"]
        );
    }

    #[test]
    fn flags_depend_on_the_verb() {
        assert_eq!(complete("kubectl get -").1, ["--namespace", "--output", "-n", "-o"]);
        assert_eq!(complete("kubectl get pods --o").1, ["--output"]);
        assert_eq!(complete("kubectl logs x --t").1, ["--tail", "--timestamps"]);
        assert_eq!(complete("kubectl logs x -").1.len(), 8);
        assert_eq!(complete("kubectl delete pod x --f").1, ["--force"]);
        assert_eq!(complete("kubectl set env deploy/web --l").1, ["--list"]);
        assert_eq!(complete("kubectl describe pod x -").1, ["--namespace", "-n"]);
    }

    #[test]
    fn output_formats() {
        assert_eq!(complete("kubectl get pods -o ").1, ["json", "name", "wide", "yaml"]);
        assert_eq!(complete("kubectl get pods --output=y").1, ["--output=yaml"]);
        assert_eq!(complete("kubectl describe pod x -o ").1, Vec::<String>::new());
    }

    #[test]
    fn logs_offers_pods_deployments_and_containers() {
        let (_, targets) = complete("kubectl logs ");
        assert!(targets.iter().any(|t| t.starts_with("payments-api-")), "{targets:?}");
        assert!(targets.contains(&"deploy/payments-api".to_string()), "{targets:?}");
        assert_eq!(complete("kubectl logs deploy/p").1, ["deploy/payments-api"]);
        assert_eq!(complete("kubectl logs deploy/payments-api -c ").1, ["payments-api"]);
        assert_eq!(
            complete("kubectl logs deploy/payments-api --container=pay").1,
            ["--container=payments-api"]
        );
    }

    #[test]
    fn set_env_completes_the_target_and_container_but_not_assignments() {
        assert_eq!(complete("kubectl set env deployment/p").1, ["deployment/payments-api"]);
        assert_eq!(complete("kubectl set env deployment ").1, ["payments-api", "web"]);
        assert_eq!(
            complete("kubectl set env deployment payments-api DATA").1,
            Vec::<String>::new()
        );
        assert_eq!(complete("kubectl set env deploy/payments-api -c ").1, ["payments-api"]);
        assert_eq!(
            complete("kubectl set env deployment payments-api -c ").1,
            ["payments-api"]
        );
    }

    #[test]
    fn quotes_and_odd_lines_yield_nothing() {
        assert_eq!(complete("kubectl get pods \"pay").1, Vec::<String>::new());
        assert_eq!(complete("kubectl set env d \"A B").1, Vec::<String>::new());
        assert_eq!(complete("kubectl get 'pods ").1, Vec::<String>::new());
        assert_eq!(complete("kubectl   get   po").1, ["pods"]);
        assert_eq!(complete("kubectl   get   po").0, 16);
    }

    #[test]
    fn completion_serializes_for_the_frontend() {
        let json = serde_json::to_string(&engine().complete("kubectl g")).unwrap();
        assert_eq!(json, r#"{"start":8,"candidates":["get"]}"#);
    }

    #[test]
    fn start_counts_characters_not_bytes() {
        assert_eq!(complete("kubectl get pods é").0, 17);
    }
}
