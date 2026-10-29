//! End-to-end: a service finds its database through the cluster. The connection goes through DNS,
//! the Service and the ready endpoints to the postgres pod, which lets the client in only if the
//! role, database and password all match (V9, V10, V32).

use engine::{AppSet, Engine, Files};

const SCENARIO: &str = r#"{"schemaVersion":2,"id":"postgres","cluster":{"now":"2026-10-27T18:30:00Z","manifests":["nodes.yaml"]},
    "images":{"api:1":{"pullSeconds":1,"app":"db-client@0.2.0","config":{"name":"api","version":"1","databaseUrlEnv":"DATABASE_URL","portEnv":"PORT"}},
              "postgres:16":{"pullSeconds":1,"app":"postgres@0.1.0","config":{}}}}"#;

fn engine() -> Engine {
    let files = Files::from([(
        "nodes.yaml".to_string(),
        "apiVersion: v1\nkind: Node\nmetadata:\n  name: worker-1\n".to_string(),
    )]);
    Engine::from_scenario(SCENARIO, &files, AppSet::bundled()).expect("scenario loads")
}

fn apply(e: &mut Engine, yaml: &str) {
    e.write_file("apply.yaml", yaml);
    let out = e.eval("kubectl apply -f apply.yaml");
    assert!(!out.starts_with("error"), "{out}");
}

const DB: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: db\n  namespace: data\n  labels: {app: postgres}\nspec:\n  containers:\n  - name: postgres\n    image: postgres:16\n    env:\n    - {name: POSTGRES_USER, value: payments}\n    - {name: POSTGRES_PASSWORD, value: s3cret}\n    - {name: POSTGRES_DB, value: payments}\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: postgres\n  namespace: data\nspec:\n  selector: {app: postgres}\n  ports:\n  - {port: 5432}\n";

fn api(url: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: api\nspec:\n  restartPolicy: Never\n  containers:\n  - name: api\n    image: api:1\n    env:\n    - {{name: PORT, value: \"8080\"}}\n    - {{name: DATABASE_URL, value: \"{url}\"}}\n"
    )
}

fn world() -> Engine {
    let mut e = engine();
    apply(&mut e, "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: data\n");
    apply(&mut e, DB);
    e.advance(10);
    e
}

fn run_api(e: &mut Engine, url: &str) -> String {
    e.eval("kubectl delete pod api --force");
    apply(e, &api(url));
    e.advance(10);
    e.eval("kubectl logs api")
}

#[test]
fn the_right_url_connects_through_the_cluster() {
    let mut e = world();
    let log = run_api(
        &mut e,
        "postgres://payments:s3cret@postgres.data.svc.cluster.local:5432/payments",
    );
    assert!(log.contains("connected to postgres://"), "{log}");
    assert!(log.contains("listening on :8080"), "{log}");
}

#[test]
fn each_way_to_get_the_url_wrong_fails_differently() {
    let mut e = world();
    let case = |e: &mut Engine, url: &str| run_api(e, url);
    // Short name from another namespace: the name does not resolve.
    assert!(case(&mut e, "postgres://payments:s3cret@postgres:5432/payments").contains("Name or service not known"));
    // Nothing listens on that port of the Service.
    assert!(case(&mut e, "postgres://payments:s3cret@postgres.data:5433/payments").contains("timeout expired"));
    // The server says no, and says why.
    assert!(case(&mut e, "postgres://payments:nope@postgres.data:5432/payments")
        .contains("password authentication failed for user \"payments\""));
    assert!(case(&mut e, "postgres://payments:s3cret@postgres.data:5432/ledger")
        .contains("database \"ledger\" does not exist"));
    assert!(case(&mut e, "postgres://bob:s3cret@postgres.data:5432/payments").contains("role \"bob\" does not exist"));
}

#[test]
fn a_database_that_is_not_ready_refuses_and_a_restart_after_the_fix_connects() {
    let mut e = engine();
    apply(&mut e, "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: data\n");
    // The password is missing, so postgres refuses to start and nothing is behind the Service.
    apply(
        &mut e,
        &DB.replace("    - {name: POSTGRES_PASSWORD, value: s3cret}\n", ""),
    );
    e.advance(10);
    assert!(e.eval("kubectl logs db -n data").contains("POSTGRES_PASSWORD"));
    let url = "postgres://payments:s3cret@postgres.data:5432/payments";
    let log = run_api(&mut e, url);
    assert!(log.contains("Connection refused"), "{log}");

    // The running api is not told when the database comes back (V32): a new run is.
    e.eval("kubectl delete pod db -n data --force");
    apply(&mut e, DB);
    e.advance(10);
    assert!(e.eval("kubectl logs api").contains("Connection refused"));
    let log = run_api(&mut e, url);
    assert!(log.contains("connected to"), "{log}");
}
