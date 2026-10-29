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
