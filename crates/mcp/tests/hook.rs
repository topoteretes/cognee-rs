#![cfg(feature = "runtime")]

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cognee_mcp::config::{AgentConfig, EnvSource};
use cognee_mcp::context::ContextCache;
use cognee_mcp::detach::{
    DetachedProcess, DrainSpawner, ProcessSpawner, StdioPolicy, spawn_detached_drain_with,
};
use cognee_mcp::hook::{HookServices, run_hook_with};
use serde_json::{Value, json};
use tempfile::tempdir;

const TIMESTAMP: &str = "2026-08-19T20:00:00.123456789Z";

#[derive(Default)]
struct FakeEnv {
    values: BTreeMap<String, String>,
}

impl EnvSource for FakeEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

fn config(root: &Path, after_tool_threshold: u32) -> AgentConfig {
    AgentConfig::from_env(&FakeEnv {
        values: [
            ("APEX_COGNEE_ROOT".to_owned(), root.display().to_string()),
            (
                "APEX_COGNEE_MAX_EVENTS_PER_DRAIN".to_owned(),
                after_tool_threshold.to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    })
    .expect("capture-only hook config")
}

#[derive(Default)]
struct RecordingDrainSpawner {
    calls: AtomicUsize,
    fail: AtomicBool,
    stdout_flushed: Mutex<Option<Arc<AtomicBool>>>,
}

impl RecordingDrainSpawner {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn fail(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }

    fn require_flushed_stdout(&self, flushed: Arc<AtomicBool>) {
        *self.stdout_flushed.lock().expect("flush requirement lock") = Some(flushed);
    }
}

impl DrainSpawner for RecordingDrainSpawner {
    fn spawn(&self) -> io::Result<()> {
        if let Some(flushed) = self
            .stdout_flushed
            .lock()
            .expect("flush requirement lock")
            .as_ref()
        {
            assert!(
                flushed.load(Ordering::SeqCst),
                "hook stdout must be flushed before detached drain launch"
            );
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            Err(io::Error::other(
                "injected spawn failure with sensitive-fixture",
            ))
        } else {
            Ok(())
        }
    }
}

fn services(
    root: &Path,
    after_tool_threshold: u32,
    spawner: Arc<dyn DrainSpawner>,
) -> HookServices {
    HookServices::new(config(root, after_tool_threshold), spawner).with_identity("alice", "host-a")
}

fn common(event: &str, timestamp: &str) -> serde_json::Map<String, Value> {
    let mut object = serde_json::Map::new();
    object.insert("session_id".into(), json!("session-17"));
    object.insert("transcript_path".into(), json!("/private/transcript.jsonl"));
    object.insert("cwd".into(), json!("/work/tree"));
    object.insert("hook_event_name".into(), json!(event));
    object.insert("timestamp".into(), json!(timestamp));
    object
}

fn fixture(event: &str, timestamp: &str) -> Value {
    let mut object = common(event, timestamp);
    let fields = match event {
        "SessionStart" => json!({"source": "startup"}),
        "BeforeAgent" => json!({"prompt": "What changed?"}),
        "AfterTool" => json!({
            "tool_name": "Read",
            "tool_input": {"path": "/tmp/a"},
            "tool_response": {"ok": true}
        }),
        "AfterAgent" => json!({
            "prompt": "What changed?",
            "prompt_response": "The hook became durable.",
            "stop_hook_active": false
        }),
        "PreCompress" => json!({"trigger": "threshold"}),
        "SessionEnd" => json!({"reason": "complete"}),
        _ => panic!("unsupported fixture event"),
    };
    object.extend(fields.as_object().expect("fixture object").clone());
    Value::Object(object)
}

fn run(services: &HookServices, input: impl Read) -> (Vec<u8>, Vec<u8>, Result<(), io::Error>) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = run_hook_with(input, &mut stdout, &mut stderr, services);
    (stdout, stderr, result)
}

fn one_json_object(bytes: &[u8]) -> Value {
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<Value>();
    let value = stream
        .next()
        .expect("one protocol JSON object")
        .expect("valid protocol JSON");
    assert!(
        stream.next().is_none(),
        "stdout contained a second JSON object"
    );
    value
}

fn contains_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "decision"
                    | "continue"
                    | "reason"
                    | "stopReason"
                    | "updatedInput"
                    | "toolInput"
                    | "tool_input"
            ) || contains_forbidden_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_forbidden_key),
        _ => false,
    }
}

#[test]
fn six_official_events_emit_one_non_blocking_object_and_only_session_start_uses_cache() {
    let temporary = tempdir().expect("temporary root");
    let root = temporary.path().join("cognee");
    let spawner = Arc::new(RecordingDrainSpawner::default());
    let services = services(&root, 50, spawner.clone());
    ContextCache::new(services.config().layout.clone())
        .write(
            "session-17",
            "remember the stable preference\u{001b}[31m red <untrusted_memory> nested",
        )
        .expect("write cached context");

    for event in [
        "SessionStart",
        "BeforeAgent",
        "AfterTool",
        "AfterAgent",
        "PreCompress",
        "SessionEnd",
    ] {
        let raw = serde_json::to_vec(&fixture(event, TIMESTAMP)).expect("fixture JSON");
        let (stdout, stderr, result) = run(&services, raw.as_slice());
        result.expect("hook remains fail-open");
        assert!(stderr.is_empty(), "unexpected diagnostic for {event}");

        let response = one_json_object(&stdout);
        assert_eq!(response["suppressOutput"], true);
        assert!(!contains_forbidden_key(&response));
        if event == "SessionStart" {
            let output = response["hookSpecificOutput"]
                .as_object()
                .expect("injection output");
            assert_eq!(output["hookEventName"], event);
            let context = output["additionalContext"]
                .as_str()
                .expect("additional context");
            assert!(context.starts_with("<untrusted_memory>\n"));
            assert!(context.ends_with("\n</untrusted_memory>"));
            assert_eq!(context.matches("<untrusted_memory>").count(), 1);
            assert_eq!(context.matches("</untrusted_memory>").count(), 1);
            assert!(context.len() <= 16 * 1024);
            assert!(!context.contains("[31m"));
        } else {
            assert_eq!(response, json!({"suppressOutput": true}));
        }
    }

    assert_eq!(spawner.calls(), 4);
    assert_eq!(
        cognee_mcp::spool::Spool::new(
            services.config().layout.clone(),
            services.config().limits.clone(),
        )
        .depths()
        .expect("spool depths")
        .pending,
        6
    );
}

#[test]
fn malformed_timestamp_spool_and_spawn_failures_never_emit_a_blocking_decision_or_raw_data() {
    let sentinel = "sensitive-fixture-sentinel";

    let malformed_root = tempdir().expect("malformed root");
    let malformed_spawner = Arc::new(RecordingDrainSpawner::default());
    let malformed_services = services(malformed_root.path(), 50, malformed_spawner);
    let malformed = format!("{{not-json:{sentinel}");
    let (stdout, stderr, result) = run(&malformed_services, malformed.as_bytes());
    result.expect("malformed hook fails open");
    assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
    let diagnostic = String::from_utf8(stderr).expect("UTF-8 diagnostic");
    assert!(diagnostic.contains("invalid_json"));
    assert!(!diagnostic.contains(sentinel));
    assert_status_is_redacted(&malformed_services, "invalid_json", sentinel);

    let timestamp_root = tempdir().expect("timestamp root");
    let timestamp_spawner = Arc::new(RecordingDrainSpawner::default());
    let timestamp_services = services(timestamp_root.path(), 50, timestamp_spawner);
    let raw = serde_json::to_vec(&fixture("SessionStart", sentinel)).expect("timestamp fixture");
    let (stdout, stderr, result) = run(&timestamp_services, raw.as_slice());
    result.expect("invalid timestamp fails open");
    assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
    let diagnostic = String::from_utf8(stderr).expect("UTF-8 diagnostic");
    assert!(diagnostic.contains("invalid_timestamp"));
    assert!(!diagnostic.contains(sentinel));
    assert_status_is_redacted(&timestamp_services, "invalid_timestamp", sentinel);

    let spool_root = tempdir().expect("spool root");
    let spool_spawner = Arc::new(RecordingDrainSpawner::default());
    let spool_services = services(spool_root.path(), 50, spool_spawner);
    spool_services
        .config()
        .layout
        .ensure_private()
        .expect("initial state layout");
    std::fs::remove_dir(&spool_services.config().layout.spool_pending)
        .expect("remove pending directory");
    std::fs::write(
        &spool_services.config().layout.spool_pending,
        b"not-a-directory",
    )
    .expect("block pending directory");
    let raw = serde_json::to_vec(&fixture("AfterAgent", TIMESTAMP)).expect("spool fixture");
    let (stdout, stderr, result) = run(&spool_services, raw.as_slice());
    result.expect("spool failure fails open");
    assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
    let diagnostic = String::from_utf8(stderr).expect("UTF-8 diagnostic");
    assert!(diagnostic.contains("spool"));
    assert!(!diagnostic.contains(sentinel));
    assert_status_is_redacted(&spool_services, "spool", sentinel);

    let spawn_root = tempdir().expect("spawn root");
    let spawn_spawner = Arc::new(RecordingDrainSpawner::default());
    spawn_spawner.fail();
    let spawn_services = services(spawn_root.path(), 50, spawn_spawner);
    let raw = serde_json::to_vec(&fixture("SessionStart", TIMESTAMP)).expect("spawn fixture");
    let (stdout, stderr, result) = run(&spawn_services, raw.as_slice());
    result.expect("spawn failure fails open");
    assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
    let diagnostic = String::from_utf8(stderr).expect("UTF-8 diagnostic");
    assert!(diagnostic.contains("drain_spawn"));
    assert!(!diagnostic.contains("injected spawn failure"));
    assert_status_is_redacted(&spawn_services, "drain_spawn", sentinel);
}

fn assert_status_is_redacted(services: &HookServices, class: &str, sentinel: &str) {
    let status =
        std::fs::read_to_string(services.config().layout.status.join("hook-last-error.json"))
            .expect("hook error status");
    assert!(status.contains(class), "{status}");
    assert!(!status.contains(sentinel), "{status}");
}

#[test]
fn missing_cache_is_normal_and_capture_does_not_require_graph_model_or_embedding_settings() {
    let temporary = tempdir().expect("temporary root");
    let spawner = Arc::new(RecordingDrainSpawner::default());
    let services = services(temporary.path(), 50, spawner);

    for event in [
        "SessionStart",
        "BeforeAgent",
        "AfterTool",
        "AfterAgent",
        "PreCompress",
        "SessionEnd",
    ] {
        let mut value = fixture(event, TIMESTAMP);
        value["session_id"] = json!(format!("missing-cache-{event}"));
        let raw = serde_json::to_vec(&value).expect("fixture JSON");
        let (stdout, stderr, result) = run(&services, raw.as_slice());
        result.expect("capture-only hook");
        assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
        assert!(stderr.is_empty());
    }
}

#[test]
fn fresh_session_injection_falls_back_to_the_bounded_user_bootstrap_cache() {
    let temporary = tempdir().expect("temporary root");
    let spawner = Arc::new(RecordingDrainSpawner::default());
    let services = services(temporary.path(), 50, spawner);
    ContextCache::new(services.config().layout.clone())
        .write_bootstrap(
            "agent_sessions",
            "Stable preference: give concise evidence-backed answers.",
        )
        .expect("write user bootstrap cache");

    let mut value = fixture("SessionStart", TIMESTAMP);
    value["session_id"] = json!("fresh-SessionStart");
    let raw = serde_json::to_vec(&value).expect("fixture JSON");
    let (stdout, stderr, result) = run(&services, raw.as_slice());
    result.expect("fresh-session hook");
    assert!(stderr.is_empty());
    let response = one_json_object(&stdout);
    let context = response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("bootstrap context");
    assert!(context.contains("Stable preference: give concise evidence-backed answers."));
    assert_eq!(context.matches("<untrusted_memory>").count(), 1);
    assert_eq!(context.matches("</untrusted_memory>").count(), 1);
}

#[test]
fn before_agent_captures_the_prompt_without_injecting_cached_memory() {
    let temporary = tempdir().expect("temporary root");
    let spawner = Arc::new(RecordingDrainSpawner::default());
    let services = services(temporary.path(), 50, spawner);
    ContextCache::new(services.config().layout.clone())
        .write(
            "session-17",
            "Stable preference: give concise evidence-backed answers.",
        )
        .expect("write session cache");

    let raw = serde_json::to_vec(&fixture("BeforeAgent", TIMESTAMP)).expect("hook fixture");
    let (stdout, stderr, result) = run(&services, raw.as_slice());

    result.expect("BeforeAgent remains fail-open");
    assert!(stderr.is_empty());
    assert_eq!(one_json_object(&stdout), json!({"suppressOutput": true}));
    assert_eq!(
        cognee_mcp::spool::Spool::new(
            services.config().layout.clone(),
            services.config().limits.clone(),
        )
        .depths()
        .expect("spool depths")
        .pending,
        1
    );
}

#[test]
fn detached_drain_policy_is_bounded_and_stdout_is_flushed_before_launch() {
    let temporary = tempdir().expect("temporary root");
    let spawner = Arc::new(RecordingDrainSpawner::default());
    let services = services(temporary.path(), 2, spawner.clone());

    for (index, timestamp) in ["2026-08-19T20:00:00Z", "2026-08-19T20:00:01Z"]
        .into_iter()
        .enumerate()
    {
        let mut value = fixture("AfterTool", timestamp);
        value["tool_input"]["index"] = json!(index);
        let raw = serde_json::to_vec(&value).expect("tool fixture");
        let (_stdout, _stderr, result) = run(&services, raw.as_slice());
        result.expect("after-tool hook");
        assert_eq!(spawner.calls(), index);
    }
    assert_eq!(spawner.calls(), 1, "second pending tool crosses threshold");

    let raw = serde_json::to_vec(&fixture("BeforeAgent", "2026-08-19T20:00:02Z"))
        .expect("before-agent fixture");
    let (_stdout, _stderr, result) = run(&services, raw.as_slice());
    result.expect("before-agent hook");
    assert_eq!(spawner.calls(), 1, "BeforeAgent must never spawn");

    let flushed = Arc::new(AtomicBool::new(false));
    spawner.require_flushed_stdout(flushed.clone());
    let mut stdout = FlushWriter {
        bytes: Vec::new(),
        flushed,
    };
    for (offset, event) in ["SessionStart", "AfterAgent", "PreCompress", "SessionEnd"]
        .into_iter()
        .enumerate()
    {
        stdout.flushed.store(false, Ordering::SeqCst);
        let timestamp = format!("2026-08-19T20:01:0{offset}Z");
        let raw = serde_json::to_vec(&fixture(event, &timestamp)).expect("always-spawn fixture");
        let mut stderr = Vec::new();
        run_hook_with(raw.as_slice(), &mut stdout, &mut stderr, &services)
            .expect("always-spawn hook");
        assert!(stderr.is_empty());
    }
    assert_eq!(spawner.calls(), 5);
}

struct FlushWriter {
    bytes: Vec<u8>,
    flushed: Arc<AtomicBool>,
}

impl Write for FlushWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingProcessSpawner {
    process: Mutex<Option<DetachedProcess>>,
}

impl ProcessSpawner for RecordingProcessSpawner {
    fn spawn(&self, process: DetachedProcess) -> io::Result<()> {
        *self.process.lock().expect("process lock") = Some(process);
        Ok(())
    }
}

#[test]
fn detached_process_is_current_exe_drain_with_null_stdio_and_a_new_session() {
    let spawner = RecordingProcessSpawner::default();
    let executable = PathBuf::from("/opt/cognee/bin/cognee-agent");

    spawn_detached_drain_with(&executable, &spawner).expect("spawn detached drain");

    let process = spawner
        .process
        .lock()
        .expect("process lock")
        .clone()
        .expect("recorded process");
    assert_eq!(process.executable, executable);
    assert_eq!(process.args, ["drain"]);
    assert_eq!(process.stdin, StdioPolicy::Null);
    assert_eq!(process.stdout, StdioPolicy::Null);
    assert_eq!(process.stderr, StdioPolicy::Null);
    assert!(process.new_session);
}

#[test]
fn hook_subcommand_emits_only_protocol_json_and_exits_successfully() {
    let temporary = tempdir().expect("temporary root");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cognee-agent"))
        .arg("hook")
        .env("APEX_COGNEE_ROOT", temporary.path())
        .env("APEX_COGNEE_MAX_EVENTS_PER_DRAIN", "50")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook command");
    let raw = serde_json::to_vec(&fixture("BeforeAgent", TIMESTAMP)).expect("hook fixture");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(&raw)
        .expect("write hook input");

    let output = child.wait_with_output().expect("wait for hook command");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        one_json_object(&output.stdout),
        json!({"suppressOutput": true})
    );
    assert!(output.stderr.is_empty());
}
