use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const COMMIT_ENV: &str = "COGNEE_RS_COMMIT";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={COMMIT_ENV}");

    let Some(manifest_dir) = env::var_os("CARGO_MANIFEST_DIR") else {
        panic!("Cargo must provide CARGO_MANIFEST_DIR");
    };
    let manifest_dir = PathBuf::from(manifest_dir);
    emit_git_rerun_paths(&manifest_dir);

    let commit = match env::var(COMMIT_ENV) {
        Ok(value) => validate_commit(&value)
            .unwrap_or_else(|| panic!("{COMMIT_ENV} must be a full 40- or 64-digit Git commit")),
        Err(env::VarError::NotPresent) => git_output(&manifest_dir, &["rev-parse", "--verify", "HEAD^{commit}"])
            .as_deref()
            .and_then(validate_commit)
            .unwrap_or_else(|| {
                println!(
                    "cargo:warning=unable to resolve Cognee-RS Git commit; reference publication will remain disabled"
                );
                "unknown".to_owned()
            }),
        Err(env::VarError::NotUnicode(_)) => {
            panic!("{COMMIT_ENV} must contain UTF-8 Git commit text")
        }
    };

    println!("cargo:rustc-env={COMMIT_ENV}={commit}");
}

fn validate_commit(value: &str) -> Option<String> {
    let value = value.trim();
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(value.to_ascii_lowercase())
    } else {
        None
    }
}

fn emit_git_rerun_paths(manifest_dir: &Path) {
    for args in [
        &["rev-parse", "--git-path", "HEAD"][..],
        &["rev-parse", "--git-path", "packed-refs"][..],
    ] {
        if let Some(path) = git_output(manifest_dir, args) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    if let Some(reference) = git_output(manifest_dir, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_output(manifest_dir, &["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
