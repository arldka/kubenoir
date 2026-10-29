//! A healthy nginx: prints its usual start-up lines and serves forever. Takes no config.

fn main() {
    println!("/docker-entrypoint.sh: Configuration complete; ready for start up");
    kit::sleep(1);
    println!("nginx: worker process started");
    kit::serve_forever();
}
