//! The pilot scenarios, each played the intended way and the other ways that should also work
//! (V20, V21).

mod common;

use engine::Engine;
use serde_json::{json, Value};

fn status(e: &Engine) -> Value {
    serde_json::to_value(e.status()).unwrap()
}

fn solved(e: &Engine) -> bool {
    status(e)["solved"] == true
}

mod new_badge {
    use super::*;

    fn engine() -> Engine {
        common::engine("new-badge")
    }

    #[test]
    fn the_case_starts_with_no_desk() {
        let mut e = engine();
        let out = e.eval("kubectl get namespaces");
        assert!(
            out.contains("default") && out.contains("kube-system") && !out.contains("homicide"),
            "{out}"
        );
        assert!(!solved(&e));
    }

    #[test]
    fn the_terminal_itself_teaches_the_command() {
        let mut e = engine();
        assert!(e.eval("kubectl --help").contains("create"));
        let help = e.eval("kubectl create --help");
        assert!(help.contains("namespace"), "{help}");
        assert!(e
            .eval("kubectl create namespace --help")
            .contains("kubectl create namespace NAME"));
    }

    #[test]
    fn every_way_to_make_the_namespace_closes_the_case() {
        for command in [
            "kubectl create namespace homicide",
            "kubectl create ns homicide",
            "apply a file",
        ] {
            let mut e = engine();
            if command == "apply a file" {
                e.write_file(
                    "desk.yaml",
                    "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: homicide\n",
                );
                e.eval("kubectl apply -f desk.yaml");
            } else {
                e.eval(command);
            }
            e.advance(10);
            assert!(solved(&e), "{command}");
        }
    }

    #[test]
    fn a_wrong_name_does_not_and_deleting_default_breaks_the_rules() {
        let mut e = engine();
        assert!(e
            .eval("kubectl create namespace Homicide")
            .starts_with("The Namespace \"Homicide\" is invalid"));
        e.eval("kubectl create namespace homicides");
        e.advance(10);
        assert!(!solved(&e));
        e.eval("kubectl create namespace homicide");
        assert_eq!(
            e.eval("kubectl create namespace homicide"),
            "Error from server (AlreadyExists): namespaces \"homicide\" already exists"
        );
    }

    #[test]
    fn the_findings_are_discoverable() {
        let mut e = engine();
        e.eval("kubectl get namespaces");
        e.eval("kubectl explain namespace");
        assert_eq!(status(&e)["findings"]["found"].as_array().unwrap().len(), 2);
        assert_eq!(status(&e)["findings"]["total"], json!(2));
    }
}

mod changing_the_story {
    use super::*;

    fn engine() -> Engine {
        common::engine("changing-the-story")
    }

    /// The case file with the story changed, as the player's editor would leave it.
    fn edited(source: &str) -> String {
        common::scenario("changing-the-story")
            .1
            .get("case/informant.yaml")
            .expect("the case file ships with the scenario")
            .replace("value: harbor", &format!("value: {source}"))
    }

    #[test]
    fn the_informant_tells_his_old_story_and_nothing_owns_him() {
        let mut e = engine();
        assert!(e
            .eval("kubectl logs informant -n homicide")
            .contains("TIP_SOURCE=harbor"));
        assert!(e.eval("cat case/informant.yaml").contains("value: harbor"));
        assert!(!solved(&e));
    }

    #[test]
    fn a_running_pod_refuses_to_change_its_environment_in_place() {
        let mut e = engine();
        e.write_file("informant.yaml", &edited("docks"));
        let out = e.eval("kubectl apply -f informant.yaml");
        assert!(
            out.contains("Forbidden") && out.contains("pod updates may not change fields other than"),
            "{out}"
        );
        assert!(e.eval("kubectl logs informant -n homicide").contains("harbor"));
        e.advance(30);
        assert!(!solved(&e));
    }

    #[test]
    fn every_way_to_recreate_the_informant_closes_the_case() {
        type Way<'a> = (&'a str, &'a dyn Fn(&mut Engine));
        let ways: [Way; 4] = [
            ("delete then run", &|e| {
                e.eval("kubectl delete pod informant -n homicide");
                e.eval("kubectl run informant -n homicide --image=informant:1.0 --labels=app=informant --env=TIP_SOURCE=docks");
            }),
            ("replace --force", &|e| {
                e.write_file("informant.yaml", &edited("docks"));
                e.eval("kubectl replace --force -f informant.yaml");
            }),
            ("delete then apply", &|e| {
                e.write_file("informant.yaml", &edited("docks"));
                e.eval("kubectl delete -f case/informant.yaml");
                e.eval("kubectl apply -f informant.yaml");
            }),
            ("delete with --force then create", &|e| {
                e.write_file("informant.yaml", &edited("docks"));
                e.eval("kubectl delete pod informant -n homicide --force");
                e.eval("kubectl create -f informant.yaml");
            }),
        ];
        for (name, play) in ways {
            let mut e = engine();
            play(&mut e);
            e.advance(30);
            assert!(solved(&e), "{name}: {}", e.eval("kubectl get pods -n homicide"));
            assert!(
                e.eval("kubectl logs informant -n homicide")
                    .contains("TIP_SOURCE=docks"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_wrong_new_story_does_not_close_it() {
        let mut e = engine();
        e.write_file("informant.yaml", &edited("dock"));
        e.eval("kubectl replace --force -f informant.yaml");
        e.advance(30);
        assert!(!solved(&e));
    }

    #[test]
    fn the_findings_are_discoverable() {
        let mut e = engine();
        e.eval("kubectl logs informant -n homicide");
        e.eval("kubectl describe pod informant -n homicide");
        assert_eq!(status(&e)["findings"]["found"].as_array().unwrap().len(), 2);
    }
}
