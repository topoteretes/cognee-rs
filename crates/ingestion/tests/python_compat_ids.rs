//! Cross-language compatibility tests for ID generation.
//!
//! All expected values were computed from the Python cognee SDK:
//!
//! ```python
//! import hashlib
//! from uuid import uuid5, NAMESPACE_OID, UUID
//!
//! content_hash = hashlib.md5(b"hello world").hexdigest()
//! # => "5eb63bbbe01eeed093cb22bb8f5acdc3"
//!
//! user_id   = UUID("550e8400-e29b-41d4-a716-446655440000")
//! tenant_id = UUID("660e8400-e29b-41d4-a716-446655440001")
//!
//! uuid5(NAMESPACE_OID, f"{content_hash}{user_id}{tenant_id}")
//! # => UUID('5a23871a-b711-595c-8b9a-77a5a235cc72')
//!
//! uuid5(NAMESPACE_OID, f"{content_hash}{user_id}")
//! # => UUID('2ac38eb6-e5e1-5928-bd21-5cb131dc4f4f')
//!
//! uuid5(NAMESPACE_OID, f"main_dataset{user_id}{tenant_id}")
//! # => UUID('babfb417-8280-5a55-b3e8-ebe37c4a10cf')
//!
//! uuid5(NAMESPACE_OID, f"main_dataset{user_id}")
//! # => UUID('1f2e53f7-a9ef-55c6-81e4-2ee952c6be68')
//! ```

use cognee_ingestion::{generate_data_id, generate_dataset_id};
use uuid::Uuid;

const USER_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TENANT_ID: &str = "660e8400-e29b-41d4-a716-446655440001";
const CONTENT_HASH: &str = "5eb63bbbe01eeed093cb22bb8f5acdc3";

fn user() -> Uuid {
    Uuid::parse_str(USER_ID).unwrap()
}
fn tenant() -> Uuid {
    Uuid::parse_str(TENANT_ID).unwrap()
}

// ── Data ID ──────────────────────────────────────────────────────────────────

#[test]
fn data_id_with_tenant_matches_python() {
    let id = generate_data_id(CONTENT_HASH, user(), Some(tenant()));
    assert_eq!(
        id,
        Uuid::parse_str("5a23871a-b711-595c-8b9a-77a5a235cc72").unwrap(),
        "generate_data_id output must match Python uuid5(NAMESPACE_OID, hash+user+tenant)"
    );
}

#[test]
fn data_id_without_tenant_matches_python() {
    let id = generate_data_id(CONTENT_HASH, user(), None);
    assert_eq!(
        id,
        Uuid::parse_str("2ac38eb6-e5e1-5928-bd21-5cb131dc4f4f").unwrap(),
        "generate_data_id output must match Python uuid5(NAMESPACE_OID, hash+user)"
    );
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
        Uuid::parse_str("1f2e53f7-a9ef-55c6-81e4-2ee952c6be68").unwrap(),
        "generate_dataset_id output must match Python uuid5(NAMESPACE_OID, name+user)"
    );
}

// ── Text file naming ─────────────────────────────────────────────────────────

#[test]
fn text_file_name_matches_python() {
    // Python: f"text_{hashlib.md5(b'hello world').hexdigest()}.txt"
    assert_eq!(
        format!("text_{}.txt", CONTENT_HASH),
        "text_5eb63bbbe01eeed093cb22bb8f5acdc3.txt"
    );
}

// ── UUID display format ───────────────────────────────────────────────────────

#[test]
fn uuid_display_format_matches_python_str() {
    // Python str(uuid) == Rust format!("{}", uuid) — both hyphenated lowercase.
    // This is the critical property that makes ID generation compatible.
    let uuid = Uuid::parse_str(USER_ID).unwrap();
    assert_eq!(format!("{}", uuid), USER_ID);
}
