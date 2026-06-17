//! Offline smoke test for the `cognee-cli bench` subcommand.
//!
//! Runs `bench --mock-llm` against a minimal cassette + corpus the test writes
//! to a temp dir. No network and no API key are required: the replay mock's
//! default `EmptyGraph` miss policy makes extraction return empty graphs and
//! summaries stubs, and `MOCK_EMBEDDING=deterministic` keeps the embedding
//! path local — so the full prune → setup → add → cognify → search pipeline
//! completes offline.
#![cfg(feature = "bench")]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_bench_help() {
    let config_home = TempDir::new().expect("temp dir");
    Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"))
        .env("XDG_CONFIG_HOME", config_home.path())
        .arg("bench")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_bench_mock_offline_smoke() {
    let dir = TempDir::new().expect("temp dir");
    let config_home = TempDir::new().expect("config home");

    // Minimal valid cassette — empty entries exercise the EmptyGraph miss path.
    let cassette = dir.path().join("cassette.json");
    fs::write(
        &cassette,
        r#"{"version":1,"model":"mock-model","entries":{}}"#,
    )
    .expect("write cassette");

    // Tiny corpus.
    let corpus = dir.path().join("memories.json");
    fs::write(
        &corpus,
        r#"[
            {"title": "Doc A", "content": "Alpha content about widgets.", "references": ["r1"]},
            {"title": "Doc B", "content": "Beta content about gadgets.", "references": []}
        ]"#,
    )
    .expect("write corpus");

    let output = dir.path().join("result.json");

    Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"))
        .env("XDG_CONFIG_HOME", config_home.path())
        // Belt-and-suspenders: the subcommand sets this itself, but keep the
        // env explicit so a future refactor can't silently break offline runs.
        .env("MOCK_EMBEDDING", "deterministic")
        .arg("bench")
        .arg("--mock-llm")
        .arg("--mock-memories")
        .arg(&cassette)
        .arg("--memories")
        .arg(&corpus)
        .arg("--output")
        .arg(&output)
        .assert()
        .success();

    // Result file must parse and carry the full Python schema.
    let raw = fs::read_to_string(&output).expect("result file written");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("result is valid JSON");

    // All six metric keys present and numeric / >= 0.
    for key in [
        "add_time_s",
        "cognify_time_s",
        "total_ingest_time_s",
        "prune_time_s",
        "db_setup_time_s",
        "search_time",
    ] {
        let n = v[key]
            .as_f64()
            .unwrap_or_else(|| panic!("missing/non-numeric {key}: {raw}"));
        assert!(n >= 0.0, "{key} must be >= 0, got {n}");
    }

    assert_eq!(
        v["memories_count"].as_u64(),
        Some(2),
        "memories_count: {raw}"
    );
    assert_eq!(
        v["success"].as_bool(),
        Some(true),
        "all phases should succeed offline: {raw}"
    );

    // Config block (Python parity).
    let config = &v["config"];
    assert_eq!(config["mock_llm"].as_bool(), Some(true));
    assert_eq!(config["dataset_name"].as_str(), Some("bench_memories"));
    assert!(config["llm_model"].is_string());
    assert!(config["embedding_model"].is_string());
    assert!(config["embedding_dimensions"].is_number());

    // Status block: every phase "success".
    let status = &v["status"];
    for phase in ["prune", "db_setup", "add", "cognify", "search"] {
        assert_eq!(
            status[phase].as_str(),
            Some("success"),
            "phase {phase} should be success: {raw}"
        );
    }
}

#[test]
fn test_bench_num_memories_truncates() {
    let dir = TempDir::new().expect("temp dir");
    let config_home = TempDir::new().expect("config home");

    let cassette = dir.path().join("cassette.json");
    fs::write(
        &cassette,
        r#"{"version":1,"model":"mock-model","entries":{}}"#,
    )
    .expect("write cassette");

    let corpus = dir.path().join("memories.json");
    fs::write(
        &corpus,
        r#"[
            {"content": "one"},
            {"content": "two"},
            {"content": "three"}
        ]"#,
    )
    .expect("write corpus");

    let output = dir.path().join("result.json");

    Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"))
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("MOCK_EMBEDDING", "deterministic")
        .arg("bench")
        .arg("--mock-llm")
        .arg("--mock-memories")
        .arg(&cassette)
        .arg("--memories")
        .arg(&corpus)
        .arg("--num-memories")
        .arg("1")
        .arg("--output")
        .arg(&output)
        .assert()
        .success();

    let raw = fs::read_to_string(&output).expect("result file");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(v["memories_count"].as_u64(), Some(1), "{raw}");
}
