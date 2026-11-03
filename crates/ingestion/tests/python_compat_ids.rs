#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cross-language compatibility tests for ID generation.
//!
//! All expected values were computed from the Python cognee SDK. Data ids are
//! not here: they are a random `uuid4` in both SDKs (Python `ingest_data`),
//! so only dataset ids are derived.
//!
//! ```python
//! import hashlib
//! from uuid import uuid5, NAMESPACE_OID, UUID
//!
//! user_id   = UUID("550e8400-e29b-41d4-a716-446655440000")
//! tenant_id = UUID("660e8400-e29b-41d4-a716-446655440001")
//!
//! uuid5(NAMESPACE_OID, f"main_dataset{user_id}{tenant_id}")
//! # => UUID('babfb417-8280-5a55-b3e8-ebe37c4a10cf')
//!
//! uuid5(NAMESPACE_OID, f"main_dataset{user_id}None")
//! # => UUID('c0b626cb-2d1a-54c1-a108-d560bb6e1597')
//! ```

use cognee_ingestion::generate_dataset_id;
use uuid::Uuid;

const USER_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TENANT_ID: &str = "660e8400-e29b-41d4-a716-446655440001";
/// `hashlib.md5(b"hello world").hexdigest()`; names the stored file.
const CONTENT_HASH: &str = "5eb63bbbe01eeed093cb22bb8f5acdc3";

fn user() -> Uuid {
    Uuid::parse_str(USER_ID).unwrap()
}
fn tenant() -> Uuid {
    Uuid::parse_str(TENANT_ID).unwrap()
}

// ── Dataset ID ───────────────────────────────────────────────────────────────

#[test]
fn dataset_id_with_tenant_matches_python() {
    let id = generate_dataset_id("main_dataset", user(), Some(tenant()));
    assert_eq!(
        id,
        Uuid::parse_str("babfb417-8280-5a55-b3e8-ebe37c4a10cf").unwrap(),
        "generate_dataset_id output must match Python uuid5(NAMESPACE_OID, name+user+tenant)"
    );
}

#[test]
fn dataset_id_without_tenant_matches_python() {
    let id = generate_dataset_id("main_dataset", user(), None);
    assert_eq!(
        id,
        Uuid::parse_str("c0b626cb-2d1a-54c1-a108-d560bb6e1597").unwrap(),
        "generate_dataset_id output must match Python uuid5(NAMESPACE_OID, name+user+None)"
    );
}

// ── Text file naming ─────────────────────────────────────────────────────────

#[test]
fn text_file_name_matches_python() {
    // Python: f"text_{hashlib.md5(b'hello world').hexdigest()}.txt"
    assert_eq!(
        format!("text_{CONTENT_HASH}.txt"),
        "text_5eb63bbbe01eeed093cb22bb8f5acdc3.txt"
    );
}

// ── UUID display format ───────────────────────────────────────────────────────

#[test]
fn uuid_display_format_matches_python_str() {
    // Python str(uuid) == Rust format!("{}", uuid) — both hyphenated lowercase.
    // This is the critical property that makes ID generation compatible.
    let uuid = Uuid::parse_str(USER_ID).unwrap();
    assert_eq!(format!("{uuid}"), USER_ID);
}
