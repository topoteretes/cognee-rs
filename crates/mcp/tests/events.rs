use cognee_mcp::event::{EventEnvelope, EventKind, HookEvent, canonical_json};
use cognee_mcp::hook_input::{HookInput, SchemaError};
use serde_json::{Value, json};

const TIMESTAMP: &str = "2026-08-18T14:03:02.123456-04:00";

fn common(event: &str) -> serde_json::Map<String, Value> {
    let mut object = serde_json::Map::new();
    object.insert("session_id".into(), json!("session-17"));
    object.insert("transcript_path".into(), json!("/private/transcript.jsonl"));
    object.insert("cwd".into(), json!("/work/tree"));
    object.insert("hook_event_name".into(), json!(event));
    object.insert("timestamp".into(), json!(TIMESTAMP));
    object
}

fn parse(value: Value) -> HookInput {
    let bytes = serde_json::to_vec(&value).unwrap();
    HookInput::parse(&bytes).unwrap()
}

#[test]
fn parses_exactly_the_six_official_hook_schemas() {
    let cases = [
        (
            "SessionStart",
            HookEvent::SessionStart,
            json!({"source": "startup"}),
        ),
        (
            "BeforeAgent",
            HookEvent::BeforeAgent,
            json!({"prompt": "question"}),
        ),
        (
            "AfterTool",
            HookEvent::AfterTool,
            json!({
                "tool_name": "Read",
                "tool_input": {"path": "/tmp/a"},
                "tool_response": {"ok": true}
            }),
        ),
        (
            "AfterAgent",
            HookEvent::AfterAgent,
            json!({
                "prompt": "question",
                "prompt_response": "answer",
                "stop_hook_active": false
            }),
        ),
        (
            "SessionEnd",
            HookEvent::SessionEnd,
            json!({"reason": "complete"}),
        ),
        (
            "PreCompress",
            HookEvent::PreCompress,
            json!({"trigger": "threshold"}),
        ),
    ];

    for (name, expected_event, event_fields) in cases {
        let mut object = common(name);
        let fields = event_fields.as_object().unwrap();
        object.extend(fields.clone());
        object.insert("env".into(), json!({"SHOULD_NOT_SURVIVE": "fixture"}));
        object.insert("unknown".into(), json!("discard me"));

        let hook = parse(Value::Object(object));
        assert_eq!(hook.event, expected_event);
        assert_eq!(hook.session_id, "session-17");
        assert_eq!(hook.transcript_path, "/private/transcript.jsonl");
        assert_eq!(hook.cwd, "/work/tree");
        assert_eq!(hook.timestamp, TIMESTAMP);
        assert_eq!(hook.payload, event_fields);
    }
}

#[test]
fn schema_errors_are_typed_and_reject_mcp_remember_at_the_hook_boundary() {
    let missing_session = json!({
        "transcript_path": "/tmp/t", "cwd": "/tmp", "hook_event_name": "BeforeAgent",
        "timestamp": TIMESTAMP, "prompt": "p"
    });
    assert!(matches!(
        HookInput::parse(&serde_json::to_vec(&missing_session).unwrap()),
        Err(SchemaError::MissingField("session_id"))
    ));

    let mut missing_event_field = common("AfterTool");
    missing_event_field.insert("tool_name".into(), json!("Read"));
    missing_event_field.insert("tool_input".into(), json!({}));
    assert!(matches!(
        HookInput::parse(&serde_json::to_vec(&missing_event_field).unwrap()),
        Err(SchemaError::MissingField("tool_response"))
    ));

    let mut invalid_timestamp = common("SessionStart");
    invalid_timestamp.insert("timestamp".into(), json!("not-rfc3339"));
    invalid_timestamp.insert("source".into(), json!("startup"));
    assert!(matches!(
        HookInput::parse(&serde_json::to_vec(&invalid_timestamp).unwrap()),
        Err(SchemaError::InvalidTimestamp)
    ));

    let mut missing_timestamp = common("SessionStart");
    missing_timestamp.remove("timestamp");
    missing_timestamp.insert("source".into(), json!("startup"));
    assert!(matches!(
        HookInput::parse(&serde_json::to_vec(&missing_timestamp).unwrap()),
        Err(SchemaError::MissingField("timestamp"))
    ));

    let mcp = Value::Object(common("McpRemember"));
    assert!(matches!(
        HookInput::parse(&serde_json::to_vec(&mcp).unwrap()),
        Err(SchemaError::UnknownHookEvent)
    ));
}

#[test]
fn canonical_json_sorts_object_keys_recursively() {
    let value = json!({"z": [{"b": 2, "a": 1}], "a": {"d": 4, "c": 3}});
    assert_eq!(
        canonical_json(&value),
        r#"{"a":{"c":3,"d":4},"z":[{"a":1,"b":2}]}"#
    );
}

fn envelope(raw: &[u8], generation: u64) -> EventEnvelope {
    let hook = HookInput::parse(raw).unwrap();
    EventEnvelope::from_hook(hook, "engineer", "host", "agent_sessions", generation)
}

#[test]
fn reordered_official_json_has_identical_hashes_in_one_hundred_cases() {
    let fields = [
        ("session_id", json!("session-17")),
        ("transcript_path", json!("/private/transcript.jsonl")),
        ("cwd", json!("/work/tree")),
        ("hook_event_name", json!("AfterTool")),
        ("timestamp", json!(TIMESTAMP)),
        ("tool_name", json!("Search")),
        (
            "tool_input",
            json!({"z": 3, "nested": {"y": 2, "x": 1}, "a": 0}),
        ),
        ("tool_response", json!({"matches": [{"b": 2, "a": 1}]})),
        ("env", json!({"NEVER": "persist"})),
    ];
    let baseline = envelope(
        &serde_json::to_vec(&Value::Object(
            fields
                .clone()
                .into_iter()
                .map(|(k, v)| (k.into(), v))
                .collect(),
        ))
        .unwrap(),
        9,
    );
    let mut state = 0x51f1_5eed_u64;

    for _case in 0..100 {
        let mut indices: Vec<usize> = (0..fields.len()).collect();
        for index in (1..indices.len()).rev() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            indices.swap(index, (state as usize) % (index + 1));
        }
        let mut raw = String::from("{");
        for (position, index) in indices.into_iter().enumerate() {
            if position != 0 {
                raw.push(',');
            }
            raw.push_str(&serde_json::to_string(fields[index].0).unwrap());
            raw.push(':');
            raw.push_str(&serde_json::to_string(&fields[index].1).unwrap());
        }
        raw.push('}');
        let candidate = envelope(raw.as_bytes(), 9);
        assert_eq!(candidate.payload_hash, baseline.payload_hash);
        assert_eq!(candidate.event_id, baseline.event_id);
    }
}

#[test]
fn source_timestamp_is_preserved_exactly_and_timestamp_or_generation_changes_id() {
    let raw = format!(
        r#"{{"session_id":"s","transcript_path":"t","cwd":"c","hook_event_name":"BeforeAgent","timestamp":"{TIMESTAMP}","prompt":"p"}}"#
    );
    let base = envelope(raw.as_bytes(), 1);
    let changed_timestamp = envelope(
        raw.replace(TIMESTAMP, "2026-08-18T18:03:02.123456Z")
            .as_bytes(),
        1,
    );
    let changed_generation = envelope(raw.as_bytes(), 2);

    assert_eq!(base.timestamp, TIMESTAMP);
    assert_ne!(base.event_id, changed_timestamp.event_id);
    assert_ne!(base.event_id, changed_generation.event_id);
    assert_eq!(base.event, EventKind::BeforeAgent);
    assert_eq!(base.event_id.len(), 64);
    assert!(
        base.event_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
}

#[test]
fn repeated_session_end_callbacks_share_one_external_idempotency_key() {
    let session_end = |timestamp: &str, cwd: &str, reason: &str| {
        envelope(
            serde_json::to_vec(&json!({
                "session_id": "479b2310-764a-4b3d-9c48-ad2dc754ebc4",
                "transcript_path": "/tmp/transcript.jsonl",
                "cwd": cwd,
                "hook_event_name": "SessionEnd",
                "timestamp": timestamp,
                "reason": reason
            }))
            .unwrap()
            .as_slice(),
            1,
        )
    };
    let first = session_end("2026-08-20T04:26:26.588Z", "/opt/apex_tracking", "exit");
    let second = session_end("2026-08-20T04:26:26.953Z", "/opt/apex_tracking", "exit");
    let third = session_end("2026-08-20T04:26:27.383Z", "/other/spelling", "exit");
    let different_reason = session_end("2026-08-20T04:26:27.383Z", "/opt/apex_tracking", "logout");

    assert_ne!(first.event_id, second.event_id);
    assert_ne!(second.event_id, third.event_id);
    assert_eq!(first.external_event_id(), second.external_event_id());
    assert_eq!(second.external_event_id(), third.external_event_id());
    assert_ne!(
        third.external_event_id(),
        different_reason.external_event_id()
    );

    let before_agent = envelope(
        br#"{"session_id":"s","transcript_path":"t","cwd":"c","hook_event_name":"BeforeAgent","timestamp":"2026-08-20T04:26:26.588Z","prompt":"p"}"#,
        1,
    );
    assert_eq!(before_agent.external_event_id(), before_agent.event_id);
}

#[test]
fn payload_limits_are_bytes_and_never_split_utf8() {
    let prompt = "é".repeat(20_000);
    let response = "🙂".repeat(10_000);
    let tool_input = "é".repeat(10_000);
    let tool_response = "🙂".repeat(10_000);

    let before = parse(json!({
        "session_id": "s", "transcript_path": "t", "cwd": "c",
        "hook_event_name": "BeforeAgent", "timestamp": TIMESTAMP, "prompt": prompt
    }));
    let before_envelope = EventEnvelope::from_hook(before, "e", "h", "d", 0);
    assert!(before_envelope.payload["prompt"].as_str().unwrap().len() <= 32 * 1024);
    assert!(before_envelope.capture.prompt_truncated);

    let after_agent = parse(json!({
        "session_id": "s", "transcript_path": "t", "cwd": "c",
        "hook_event_name": "AfterAgent", "timestamp": TIMESTAMP,
        "prompt": "small", "prompt_response": response, "stop_hook_active": true
    }));
    let after_agent_envelope = EventEnvelope::from_hook(after_agent, "e", "h", "d", 0);
    assert!(
        after_agent_envelope.payload["prompt_response"]
            .as_str()
            .unwrap()
            .len()
            <= 32 * 1024
    );
    assert!(after_agent_envelope.capture.response_truncated);

    let after_tool = parse(json!({
        "session_id": "s", "transcript_path": "t", "cwd": "c",
        "hook_event_name": "AfterTool", "timestamp": TIMESTAMP, "tool_name": "Run",
        "tool_input": tool_input, "tool_response": tool_response
    }));
    let after_tool_envelope = EventEnvelope::from_hook(after_tool, "e", "h", "d", 0);
    assert!(
        after_tool_envelope.payload["tool_input"]
            .as_str()
            .unwrap()
            .len()
            <= 16 * 1024
    );
    assert!(
        after_tool_envelope.payload["tool_response"]
            .as_str()
            .unwrap()
            .len()
            <= 32 * 1024
    );
    assert!(after_tool_envelope.capture.tool_input_truncated);
    assert!(after_tool_envelope.capture.tool_response_truncated);
    assert_eq!(after_tool_envelope.capture.truncation_count, 2);
}

#[test]
fn oversized_structured_tool_input_stays_structured_and_is_bounded() {
    let after_tool = parse(json!({
        "session_id": "s", "transcript_path": "t", "cwd": "c",
        "hook_event_name": "AfterTool", "timestamp": TIMESTAMP, "tool_name": "Run",
        "tool_input": {"safe": 7, "nested": {"large": "x".repeat(20_000)}},
        "tool_response": "ok"
    }));
    let envelope = EventEnvelope::from_hook(after_tool, "e", "h", "d", 0);

    assert!(envelope.payload["tool_input"].is_object());
    assert_eq!(envelope.payload["tool_input"]["safe"], 7);
    assert!(canonical_json(&envelope.payload["tool_input"]).len() <= 16 * 1024);
    assert!(envelope.capture.tool_input_truncated);
}
