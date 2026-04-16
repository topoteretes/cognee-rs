use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn make_cmd(config_home: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    command.env("XDG_CONFIG_HOME", config_home.path());
    command
}

#[test]
fn test_memify_help() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .arg("memify")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}
