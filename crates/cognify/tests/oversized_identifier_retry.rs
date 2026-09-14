//! An oversized extracted identifier must re-ask, not be accepted (SDK-629).
//!
//! The unit tests in `fact_extraction::models` pin that the bound rejects the
//! payload. They cannot pin what a rejection *costs*, because the corrective
//! retry ladder lives in the adapter, not in the model. These tests drive a real
//! [`OpenAIAdapter`] against a local `httpmock` endpoint, through the same
//! [`FactExtractor`] cognify uses, and assert the two things the ticket asked
//! for: the chunk fails validation, and the ladder re-asks with the failure
//! reason attached.
//!
//! The payload under test is the one captured during the Bedrock runaway
//! investigation (PR #210), reduced to its essential shape: valid JSON,
//! `finish_reason` a normal stop, and a single `Edge.target_node_id` of 22,305
//! characters — 94% of that answer.
//!
//! ## What a permanently-failing chunk costs
//!
//! VERIFIED in `tasks.rs` (the graph-extraction stage) and `failure.rs`, on the
//! shipped defaults — `FailureStop::FailFast`, `RollbackScope::WholeRun`:
//!
//! 1. Every chunk in the batch is dispatched before any result is inspected
//!    (`buffer_unordered` then `collect`), and `chunks_per_batch` defaults to
//!    2000 — so for any ordinary document the whole run's LLM spend is already
//!    paid when the failure is seen. The failing chunk itself costs
//!    `llm_max_retries` attempts (3, post-SDK-624).
//! 2. The chunk's failure is *recorded*, not propagated: a `StageFailure` with
//!    `fails_item: true`.
//! 3. `batch_failed` under `FailFast` sets `aborted_at` and breaks the loop.
//! 4. Because the default scope is not `FailedItems`, the stage returns
//!    `CognifyError::RunFailed` — **the whole run fails and is rolled back**,
//!    not the chunk alone. `chunk_failure_ratio_threshold` (0.05) never enters
//!    into it; that threshold is only read under `RollbackScope::FailedItems`.
//!
//! That is deliberate and it is Python's behaviour (Python rolls the whole run
//! back when any chunk raises), which is why the chunk-failure-tolerance branch
//! was dropped rather than merged. The cost is real, though, and it is the
//! reason the bound is set far from the legitimate range rather than snugly
//! around it: a false rejection is not a lost node, it is a lost run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use std::sync::Arc;

use cognee_cognify::FactExtractor;
use cognee_llm::OpenAIAdapter;
use httpmock::prelude::*;

/// The length measured in the captured degenerate payload.
const RUNAWAY_LEN: usize = 22_305;

/// Body of an OpenAI tool-call response whose `arguments` is `payload_json`.
/// Mirrors the helper in `cognee-llm`'s own structured-output tests.
fn tool_call_response(payload_json: &str) -> String {
    let escaped = serde_json::to_string(payload_json).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","tool_calls":[
                {{"id":"c1","type":"function","function":{{
                    "name":"extract_structured_data","arguments":{escaped}
                }}}}
            ]}},"finish_reason":"tool_calls"}}]}}"#
    )
}

/// A graph whose single edge points at a `RUNAWAY_LEN`-character node id.
fn degenerate_graph_payload() -> String {
    serde_json::json!({
        "nodes": [
            {"id": "Alice", "name": "Alice", "type": "PERSON", "description": "A girl."}
        ],
        "edges": [{
            "source_node_id": "Alice",
            "target_node_id": "a".repeat(RUNAWAY_LEN),
            "relationship_name": "relates_to",
            "description": "A runaway generation."
        }]
    })
    .to_string()
}

/// The same graph with an identifier a model would actually produce.
fn clean_graph_payload() -> String {
    serde_json::json!({
        "nodes": [
            {"id": "Alice", "name": "Alice", "type": "PERSON", "description": "A girl."},
            {"id": "Wonderland", "name": "Wonderland", "type": "PLACE", "description": "A place."}
        ],
        "edges": [{
            "source_node_id": "Alice",
            "target_node_id": "Wonderland",
            "relationship_name": "visits",
            "description": "Alice visits Wonderland."
        }]
    })
    .to_string()
}

fn adapter(base_url: String) -> Arc<OpenAIAdapter> {
    Arc::new(
        OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(base_url))
            .unwrap()
            // No transport retries: every request this test counts is a
            // structured-output re-ask, never a network one.
            .with_network_retries(0)
            .with_structured_output_retries(3),
    )
}

/// The ladder re-asks, and the re-ask carries the reason — so a model that
/// recovers on the second attempt still yields a graph.
#[tokio::test]
async fn an_oversized_identifier_is_re_asked_and_the_retry_succeeds() {
    let server = MockServer::start_async().await;

    // Attempt 1: no corrective marker in the request body yet → runaway payload.
    let degenerate = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("failed validation");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(&degenerate_graph_payload()));
        })
        .await;

    // Attempt 2: the corrective instruction is present, and it names the field
    // and the bound — the model is told what to fix, not merely that it failed.
    //
    // `1024` is spelled out rather than read from `MAX_IDENTIFIER_CHARS`, which
    // is private to the crate. That coupling is deliberate: changing the bound
    // should have to come past this assertion, because the number is a decision
    // (see the constant's docs), not an implementation detail.
    let corrected = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("failed validation")
                .body_includes("edges[].target_node_id")
                .body_includes("1024-character limit");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(&clean_graph_payload()));
        })
        .await;

    let extractor = FactExtractor::new(adapter(server.base_url()));
    let graph = extractor
        .extract_facts("Alice went to Wonderland.", None)
        .await
        .expect("the second, clean response must satisfy the validator");

    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);
    assert_eq!(graph.edges[0].target_node_id, "Wonderland");

    // Exactly one re-ask fired: the runaway was rejected, not accepted and not
    // silently repaired by dropping the edge.
    degenerate.assert_calls_async(1).await;
    corrected.assert_calls_async(1).await;
}

/// A model that keeps producing the runaway exhausts the ladder and fails the
/// chunk, rather than the graph quietly taking a 22,305-character node id.
#[tokio::test]
async fn an_unrecoverable_oversized_identifier_fails_the_chunk() {
    let server = MockServer::start_async().await;

    // Every attempt answers with the runaway.
    //
    // MEASURED: exactly `structured_output_retries` (3) requests, with no
    // cascade into the legacy function-calling / JSON-mode fallbacks. That is
    // the adapter distinguishing a `ValidationMiss` — the endpoint plainly
    // speaks tool calling and returned parseable JSON, it just said something
    // we reject — from `NoUsableOutput`, which is what triggers the cascade.
    // Worth pinning exactly: if a rejection ever started falling through the
    // modes, one runaway chunk would cost 3 re-asks *per mode* instead of 3.
    let always_degenerate = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(&degenerate_graph_payload()));
        })
        .await;

    let extractor = FactExtractor::new(adapter(server.base_url()));
    let err = extractor
        .extract_facts("Alice went to Wonderland.", None)
        .await
        .expect_err("a payload that never stops being degenerate must fail the chunk");

    let msg = err.to_string();
    assert!(
        msg.contains("edges[].target_node_id"),
        "the surfaced error must name the offending field, got: {msg}"
    );
    always_degenerate.assert_calls_async(3).await;
}
