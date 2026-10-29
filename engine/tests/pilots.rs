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

mod sealed_envelope {
    use super::*;

    fn engine() -> Engine {
        common::engine("sealed-envelope")
    }

    fn secret(data: &str) -> String {
        format!("apiVersion: v1\nkind: Secret\nmetadata:\n  name: db-credentials\n{data}")
    }

    fn api_status(e: &mut Engine) -> String {
        let row = e.eval("kubectl get pods --no-headers");
        row.lines()
            .find(|l| l.starts_with("payments-api"))
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn the_service_cannot_start_without_its_envelope() {
        let mut e = engine();
        assert!(
            api_status(&mut e).contains("CreateContainerConfigError"),
            "{}",
            api_status(&mut e)
        );
        let events = e.eval("kubectl get events");
        assert!(events.contains("secret \"db-credentials\" not found"), "{events}");
        assert!(e.eval("cat case/sticky-note.txt").contains("correct-horse"));
        assert!(e.eval("kubectl get pods").contains("postgres"));
        assert!(!solved(&e));
    }

    #[test]
    fn every_way_to_seal_the_password_exactly_closes_the_case() {
        let ways: [(&str, String); 5] = [
            (
                "create generic",
                "kubectl create secret generic db-credentials --from-literal=password=correct-horse".into(),
            ),
            (
                "manifest with data",
                secret("data:\n  password: Y29ycmVjdC1ob3JzZQ==\n"),
            ),
            (
                "manifest with stringData",
                secret("stringData:\n  password: correct-horse\n"),
            ),
            ("from a file", "file".into()),
            ("echo -n", "echo -n".into()),
        ];
        for (name, how) in ways {
            let mut e = engine();
            if how.starts_with("kubectl") {
                e.eval(&how);
            } else if how == "file" {
                e.eval("echo -n correct-horse > pw.txt");
                e.eval("kubectl create secret generic db-credentials --from-file=password=pw.txt");
            } else if how == "echo -n" {
                let encoded = e.eval("echo -n correct-horse | base64");
                e.write_file("secret.yaml", &secret(&format!("data:\n  password: {encoded}\n")));
                e.eval("kubectl apply -f secret.yaml");
            } else {
                e.write_file("secret.yaml", &how);
                e.eval("kubectl apply -f secret.yaml");
            }
            e.advance(60);
            assert!(
                solved(&e),
                "{name}: {}\n{}",
                api_status(&mut e),
                e.eval("kubectl logs deploy/payments-api")
            );
        }
    }

    #[test]
    fn the_newline_that_echo_adds_gets_the_service_refused_and_is_visible_in_its_log() {
        let mut e = engine();
        let encoded = e.eval("echo correct-horse | base64");
        assert_eq!(encoded, "Y29ycmVjdC1ob3JzZQo=");
        e.write_file("secret.yaml", &secret(&format!("data:\n  password: {encoded}\n")));
        e.eval("kubectl apply -f secret.yaml");
        e.advance(30);
        assert!(!solved(&e));
        let log = e.eval("kubectl logs deploy/payments-api");
        // The password carries a line break: the URL in the log is cut in two.
        assert!(
            log.contains("connecting to postgres://payments:correct-horse\n@postgres:5432/payments"),
            "{log}"
        );
        assert!(
            log.contains("password authentication failed for user \"payments\""),
            "{log}"
        );

        // Putting the right bytes in the Secret heals it at the next restart (V12, V15).
        e.write_file("secret.yaml", &secret("stringData:\n  password: correct-horse\n"));
        e.eval("kubectl apply -f secret.yaml");
        e.advance(60);
        assert!(solved(&e), "{}", api_status(&mut e));
    }

    #[test]
    fn a_wrong_password_is_not_enough() {
        let mut e = engine();
        e.eval("kubectl create secret generic db-credentials --from-literal=password=wrong-horse");
        e.advance(60);
        assert!(!solved(&e));
    }

    #[test]
    fn the_findings_are_discoverable() {
        let mut e = engine();
        e.eval("kubectl get events");
        e.eval("cat case/sticky-note.txt");
        e.eval("kubectl create secret generic db-credentials --from-literal=password=correct-horse");
        e.eval("kubectl get secret db-credentials");
        assert_eq!(status(&e)["findings"]["found"].as_array().unwrap().len(), 3);
    }
}

mod wrong_mugshot {
    use super::*;

    fn engine() -> Engine {
        common::engine("wrong-mugshot")
    }

    const GOOD_TAG: &str = "kubectl set image deployment/evidence-api evidence-api=registry.example.com/evidence:2.4.1";
    const SECRET: &str = "kubectl create secret docker-registry registry-login --docker-server=registry.example.com --docker-username=scanner --docker-password=s3cr3t-badge";
    const PATCH: &str = r#"kubectl patch deployment badge-scanner -p '{"spec":{"template":{"spec":{"imagePullSecrets":[{"name":"registry-login"}]}}}}'"#;

    fn statuses(e: &mut Engine) -> Vec<String> {
        e.eval("kubectl get pods --no-headers")
            .lines()
            .map(|l| l.split_whitespace().nth(2).unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn neither_service_has_a_running_pod_and_the_events_say_why() {
        let mut e = engine();
        let s = statuses(&mut e);
        assert_eq!(s.len(), 3, "{s:?}");
        assert!(
            s.iter().all(|s| s == "ImagePullBackOff" || s == "ErrImagePull"),
            "{s:?}"
        );
        let evidence = e.eval("kubectl describe pods -l app=evidence-api");
        assert!(
            evidence.contains("registry.example.com/evidence:2.4.l") && evidence.contains("not found"),
            "{evidence}"
        );
        let badge = e.eval("kubectl describe pods -l app=badge-scanner");
        assert!(badge.contains("authorization failed"), "{badge}");
        assert!(!solved(&e));
    }

    #[test]
    fn fixing_the_tag_and_adding_the_pull_secret_closes_the_case() {
        let mut e = engine();
        for command in [GOOD_TAG, SECRET, PATCH] {
            let out = e.eval(command);
            assert!(
                !out.starts_with("error") && !out.starts_with("Error"),
                "{command}: {out}"
            );
        }
        e.advance(90);
        assert!(solved(&e), "{:?}", statuses(&mut e));
    }

    #[test]
    fn the_same_fixes_made_with_other_commands_also_close_it() {
        let mut e = engine();
        e.eval(r#"kubectl patch deployment evidence-api -p '{"spec":{"template":{"spec":{"containers":[{"name":"evidence-api","image":"registry.example.com/evidence:2.4.1"}]}}}}'"#);
        // A pull secret written as a manifest, with the login as a base64 `auth` pair.
        let auth = e.eval("echo -n scanner:s3cr3t-badge | base64");
        let config = format!(r#"{{"auths":{{"registry.example.com":{{"auth":"{auth}"}}}}}}"#);
        let encoded = e.eval(&format!("echo -n '{config}' | base64")).replace('\n', "");
        e.write_file(
            "login.yaml",
            &format!("apiVersion: v1\nkind: Secret\nmetadata:\n  name: login\ntype: kubernetes.io/dockerconfigjson\ndata:\n  .dockerconfigjson: {encoded}\n"),
        );
        assert_eq!(e.eval("kubectl apply -f login.yaml"), "secret/login created");
        e.eval(&PATCH.replace("registry-login", "login"));
        e.advance(90);
        assert!(solved(&e), "{:?}", statuses(&mut e));
    }

    #[test]
    fn half_a_fix_or_the_wrong_login_is_not_enough() {
        let mut only_tag = engine();
        only_tag.eval(GOOD_TAG);
        only_tag.advance(120);
        assert!(!solved(&only_tag));

        let mut wrong_login = engine();
        for command in [GOOD_TAG, &SECRET.replace("s3cr3t-badge", "guess"), PATCH] {
            wrong_login.eval(command);
        }
        wrong_login.advance(120);
        assert!(!solved(&wrong_login));
        assert!(wrong_login
            .eval("kubectl describe pods -l app=badge-scanner")
            .contains("authorization failed"));
    }

    #[test]
    fn the_findings_are_discoverable() {
        let mut e = engine();
        let name = e
            .eval("kubectl get pods -o name")
            .lines()
            .map(|l| l.trim_start_matches("pod/").to_string())
            .find(|n| n.starts_with("evidence-api-"))
            .unwrap();
        e.eval(&format!("kubectl describe pod {name}"));
        e.eval("cat case/registry-note.txt");
        assert_eq!(status(&e)["findings"]["found"].as_array().unwrap().len(), 2);
    }
}
