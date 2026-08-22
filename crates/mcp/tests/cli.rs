use clap::{CommandFactory, Parser};
use cognee_mcp::cli::{Cli, Command};
use cognee_mcp::reference::ReferenceCommand;

#[test]
fn parses_all_five_top_level_commands() {
    for command in ["mcp", "hook", "drain", "doctor", "recover"] {
        assert!(
            Cli::try_parse_from(["cognee-agent", command]).is_ok(),
            "{command}"
        );
    }
}

#[test]
fn parses_read_only_recall_diagnostic_arguments() {
    let cli = Cli::try_parse_from([
        "cognee-agent",
        "recall",
        "--query",
        "stable preferences",
        "--session-id",
        "session-123",
        "--search-type",
        "CHUNKS",
        "--top-k",
        "7",
    ])
    .expect("parse recall diagnostic");

    match cli.command {
        Command::Recall {
            query,
            session_id,
            search_type,
            top_k,
        } => {
            assert_eq!(query, "stable preferences");
            assert_eq!(session_id.as_deref(), Some("session-123"));
            assert_eq!(search_type, "CHUNKS");
            assert_eq!(top_k, 7);
        }
        command => panic!("expected recall command, got {command:?}"),
    }
}

#[test]
fn reference_command_is_hidden_from_root_help_but_remains_parseable() {
    let help = Cli::command().render_long_help().to_string();
    assert!(!help.contains("reference"), "{help}");

    let cli = Cli::try_parse_from(["cognee-agent", "reference", "doctor", "--json"])
        .expect("parse hidden reference command");
    assert!(matches!(
        cli.command,
        Command::Reference {
            command: ReferenceCommand::Doctor { json: true }
        }
    ));
}

#[test]
fn mcp_reference_tool_names_are_not_cli_commands() {
    for tool in [
        "cognee_reference_recall",
        "cognee_reference_remember",
        "cognee_reference_publish",
        "cognee_reference_recover",
        "cognee_reference_forget",
    ] {
        assert!(
            Cli::try_parse_from(["cognee-agent", tool]).is_err(),
            "MCP tool leaked into the CLI surface: {tool}"
        );
    }
}

#[test]
fn reference_remember_accepts_repeated_file_aliases_without_selecting_stdin() {
    let cli = Cli::try_parse_from([
        "cognee-agent",
        "reference",
        "remember",
        "-f",
        "standards.md",
        "--file",
        "runbook.md",
        "--wait-cognified",
        "--wait-seconds",
        "7200",
    ])
    .expect("parse file ingestion");

    let Command::Reference {
        command: ReferenceCommand::Remember(arguments),
    } = cli.command
    else {
        panic!("expected reference remember command");
    };
    assert_eq!(
        arguments.files,
        vec![
            std::path::PathBuf::from("standards.md"),
            std::path::PathBuf::from("runbook.md")
        ]
    );
    assert!(!arguments.uses_stdin());
    assert!(arguments.wait_cognified);
    assert_eq!(arguments.wait_seconds, 7200);
}

#[test]
fn reference_remember_selects_stdin_only_when_files_are_absent() {
    let cli = Cli::try_parse_from([
        "cognee-agent",
        "reference",
        "remember",
        "--source-id",
        "fleet-standard",
        "--label",
        "Storage standard",
    ])
    .expect("parse stdin ingestion");

    let Command::Reference {
        command: ReferenceCommand::Remember(arguments),
    } = cli.command
    else {
        panic!("expected reference remember command");
    };
    assert!(arguments.uses_stdin());
    assert_eq!(arguments.source_id.as_deref(), Some("fleet-standard"));
    assert_eq!(arguments.label.as_deref(), Some("Storage standard"));
    assert_eq!(arguments.wait_seconds, 1800);
}

#[test]
fn reference_remember_rejects_stdin_identity_with_files_and_invalid_wait_bounds() {
    for arguments in [
        vec![
            "cognee-agent",
            "reference",
            "remember",
            "-f",
            "standards.md",
            "--source-id",
            "fleet-standard",
        ],
        vec![
            "cognee-agent",
            "reference",
            "remember",
            "--wait-seconds",
            "0",
        ],
        vec![
            "cognee-agent",
            "reference",
            "remember",
            "--wait-seconds",
            "7201",
        ],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
}

#[test]
fn parses_reference_publish_and_recovery_commands() {
    let publish =
        Cli::try_parse_from(["cognee-agent", "reference", "publish"]).expect("parse publish");
    assert!(matches!(
        publish.command,
        Command::Reference {
            command: ReferenceCommand::Publish
        }
    ));

    let recover = Cli::try_parse_from(["cognee-agent", "reference", "recover", "--adopt-orphans"])
        .expect("parse recovery");
    assert!(matches!(
        recover.command,
        Command::Reference {
            command: ReferenceCommand::Recover {
                adopt_orphans: true
            }
        }
    ));
}

#[test]
#[cfg(feature = "engine")]
fn reference_publish_command_runs_the_bounded_worker() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cognee-agent"))
        .args(["reference", "publish"])
        .env_clear()
        .env("HOME", temporary.path())
        .env(
            "APEX_COGNEE_REFERENCE_ROOT",
            temporary.path().join("reference"),
        )
        .env("APEX_COGNEE_PROXY_KEY", "fixture-key")
        .env("APEX_COGNEE_LLM_PROVIDER", "openai")
        .env("APEX_COGNEE_LLM_ENDPOINT", "https://proxy.example/v1")
        .env("APEX_COGNEE_LLM_MODEL", "gpt-5.4-mini")
        .env("APEX_COGNEE_EMBEDDING_PROVIDER", "openai")
        .env("APEX_COGNEE_EMBEDDING_ENDPOINT", "https://proxy.example/v1")
        .env("APEX_COGNEE_EMBEDDING_MODEL", "text-embedding-3-large")
        .env("APEX_COGNEE_EMBEDDING_DIMENSIONS", "3072")
        .output()
        .expect("run reference publisher");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("publisher JSON receipt");
    assert_eq!(report["caught_up"], true);
    assert_eq!(report["committed_head"], 0);
    assert_eq!(report["included_through"], 0);
}

#[test]
#[cfg(feature = "engine")]
fn drain_command_runs_an_empty_bounded_worker_without_opening_storage() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().join("cognee");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cognee-agent"))
        .arg("drain")
        .env_clear()
        .env("APEX_COGNEE_ROOT", &root)
        .env("APEX_COGNEE_PROXY_KEY", "fixture-secret")
        .env("APEX_COGNEE_LLM_PROVIDER", "openai")
        .env("APEX_COGNEE_LLM_ENDPOINT", "https://proxy.example/v1")
        .env("APEX_COGNEE_LLM_MODEL", "gpt-5.4-nano")
        .env("APEX_COGNEE_EMBEDDING_PROVIDER", "openai")
        .env(
            "APEX_COGNEE_EMBEDDING_ENDPOINT",
            "https://proxy.example/v1/embeddings",
        )
        .env("APEX_COGNEE_EMBEDDING_MODEL", "text-embedding-3-large")
        .env("APEX_COGNEE_EMBEDDING_DIMENSIONS", "3072")
        .output()
        .expect("run drain command");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(root.join("spool/pending").is_dir());
    assert!(root.join("ledger/ingestion.sqlite3").is_file());
    assert!(!root.join("locks/engine").exists());
    assert!(
        !walk_files(&root)
            .iter()
            .any(|path| { path.file_name().is_some_and(|name| name == "cognee.db") }),
        "an empty drain must not warm Cognee storage"
    );
}

#[cfg(feature = "engine")]
fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
