//! Resolves a parsed `Command` against the cluster store and renders the result.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde_json::Value;

use crate::clock;
use crate::controllers::Env;
use crate::describe::describe;
use crate::judge::Touched;
use crate::logs::logs;
use crate::manifests::{self, Files};
use crate::mutate;
use crate::parser::{Command, GetArgs, OutputFormat};
use crate::render::render;
use crate::scenario::DEFAULT_NAMESPACE;
use crate::store::{ClusterStore, StoreError};

/// Run a command: what to print, and the objects it wrote or deleted when it acts on files.
pub fn execute(command: &Command, env: &Env<'_>, files: &Files) -> (String, Vec<Touched>) {
    let printed = |text: String| (text, Vec::new());
    let written = |w: manifests::Written| (w.output, w.touched);
    let result = match command {
        Command::Get(args) => get(args, env.store, &clock::to_time(env.now)).map(printed),
        Command::Describe(args) => describe(args, env.store, env.now).map(printed),
        Command::Logs(args) => logs(args, env.store).map(printed),
        Command::SetEnv(args) => mutate::set_env(args, env).map(printed),
        Command::RolloutRestart(args) => mutate::rollout_restart(args, env).map(printed),
        Command::Delete(args) => mutate::delete(args, env).map(printed),
        Command::Apply(args) => manifests::apply(args, env, files).map(written),
        Command::Create(args) => manifests::create(args, env, files).map(written),
        Command::DeleteFiles(args) => manifests::delete(args, env, files).map(written),
        Command::Replace(args) => manifests::replace(args, env, files).map(written),
        Command::Patch(args) => crate::patch::patch(args, env),
    };
    result.unwrap_or_else(|e| (format!("error: {e}"), Vec::new()))
}

fn get(args: &GetArgs, store: &ClusterStore, now: &Time) -> Result<String, StoreError> {
    let namespace = args.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let kind = args.resource.kind();

    let mut items: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if args.names.is_empty() {
        items = store.list(kind, namespace)?;
    } else {
        for name in &args.names {
            match store.get(kind, namespace, name)? {
                Some(manifest) => items.push(manifest),
                None => errors.push(format!(
                    "Error from server (NotFound): {} \"{name}\" not found",
                    args.resource.plural()
                )),
            }
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if items.is_empty() && args.names.is_empty() && matches!(args.output, OutputFormat::Table | OutputFormat::Wide) {
        sections.push(format!("No resources found in {namespace} namespace."));
    } else if !items.is_empty() || args.names.is_empty() {
        let single = args.names.len() == 1;
        sections.push(render(args.resource, &items, args.output, single, now));
    }
    sections.extend(errors);

    Ok(sections.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::scenario::inline::load;

    const SCENARIO: &str = r#"{
        "cluster": {
            "now": "2026-10-27T18:30:00Z",
            "pods": [
                {"apiVersion":"v1","kind":"Pod","metadata":{"name":"b","creationTimestamp":"2026-10-24T18:30:00Z"},"spec":{"containers":[{"name":"c","image":"nginx:1.27"}]},"status":{"phase":"Running"}},
                {"apiVersion":"v1","kind":"Pod","metadata":{"name":"a","creationTimestamp":"2026-10-27T18:00:00Z"},"spec":{"containers":[{"name":"c","image":"nginx:1.27"}]},"status":{"phase":"Running"}}
            ]
        }
    }"#;

    fn run(cmd: &str) -> String {
        let cluster = load(SCENARIO, crate::apps::AppSet::bundled()).unwrap();
        let env = Env {
            store: &cluster.store,
            config: &cluster.config,
            now: clock::secs(&cluster.now),
        };
        execute(&parse(cmd).unwrap(), &env, &Files::new()).0
    }

    #[test]
    fn lists_pods_sorted_by_name() {
        let out = run("kubectl get pods");
        let names: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn empty_namespace_message_only_for_tables() {
        assert_eq!(
            run("kubectl get pods -n ghost"),
            "No resources found in ghost namespace."
        );
        assert_eq!(
            run("kubectl get pods -n ghost -o wide"),
            "No resources found in ghost namespace."
        );
        assert_eq!(run("kubectl get pods -n ghost -o name"), "");
        assert!(run("kubectl get pods -n ghost -o json").contains("\"kind\": \"List\""));
    }

    #[test]
    fn get_by_name() {
        let out = run("kubectl get pod a");
        assert_eq!(out.lines().count(), 2);
        assert!(out.lines().nth(1).unwrap().starts_with("a "));
    }

    #[test]
    fn not_found_error_message() {
        assert_eq!(
            run("kubectl get pod nope"),
            "Error from server (NotFound): pods \"nope\" not found"
        );
        assert_eq!(
            run("kubectl get event nope"),
            "Error from server (NotFound): events \"nope\" not found"
        );
    }

    #[test]
    fn partial_results_then_errors() {
        let out = run("kubectl get pods a nope");
        assert!(out.starts_with("NAME"));
        assert!(out.ends_with("Error from server (NotFound): pods \"nope\" not found"));
    }

    #[test]
    fn json_by_name_is_bare_object() {
        assert!(run("kubectl get pod a -o json").contains("\"kind\": \"Pod\""));
    }

    #[test]
    fn no_events_message() {
        assert_eq!(run("kubectl get events"), "No resources found in default namespace.");
    }
}
