//! Integration test: `POST /api/v1/cognify` with `run_in_background=false`.
//!
//! Requires an OpenAI-compatible LLM endpoint (`OPENAI_URL` + `OPENAI_TOKEN`)
//! and a backing database.  Skips gracefully when those env vars are absent,
//! matching the project's existing test convention.
//!
//! Per p3-pipelines-and-websocket.md §5:
//! "Pre-add a tiny dataset, then cognify.  Assert response shape:
//!  `Map<dataset_id_str, PipelineRunInfoDTO>` with `status="PipelineRunCompleted"`
//!  and `payload` being a non-empty graph snapshot."

mod support;

#[tokio::test]
async fn post_cognify_blocking_skips_without_openai() {
    // Gate: requires OPENAI_URL and OPENAI_TOKEN.
    if std::env::var("OPENAI_URL").is_err() || std::env::var("OPENAI_TOKEN").is_err() {
        eprintln!(
            "test_cognify_blocking: skipping — OPENAI_URL / OPENAI_TOKEN not set \
             (set both to run this LLM-dependent test)"
        );
        return;
    }

    // Full end-to-end test (LLM available):
    // 1. Build a state with real backends (DB + storage + LLM).
    // 2. POST /api/v1/add to seed a tiny text dataset.
    // 3. POST /api/v1/cognify with run_in_background=false.
    // 4. Assert response: Map<dataset_id_str, PipelineRunInfoDTO> with
    //    status="PipelineRunCompleted".
    //
    // TODO(P5): wire real cognify() once ComponentHandles exposes LLM/graph/vector.
    // For now this test skeleton documents the expected shape.
    todo!(
        "wire real cognify() via ComponentHandles and assert response shape \
         once P5 backend wiring is complete"
    );
}
