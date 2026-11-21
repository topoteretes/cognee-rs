//! Names shared by the code-repository ingestion (`cognee-ingestion`), the
//! cognify router (`cognee-cognify`) and the code graph load
//! (`cognee-code-graph`), defined once here because none of those crates may
//! depend on all of the others.

/// The `system_metadata.source` stamp of a code-repository manifest
/// (Python's `"code_repo"` route stamp).
pub const SOURCE_CODE_REPO: &str = "code_repo";

/// The `kind` of a code-repository manifest document (Python's
/// `build_repo_manifest`).
pub const CODE_REPO_MANIFEST_KIND: &str = "code_repo_manifest";

/// The `system_metadata.source` stamp of a single code file (Python's
/// `"code"` route stamp, written by `code_loader`).
pub const SOURCE_CODE: &str = "code";

/// The `source` of a `system_metadata` JSON text, or `None` when there is no
/// metadata, it is not a JSON object, or its `source` is not a string
/// (Python `dlt_utils.metadata_source`). A Python-written `null` reads as
/// none. The cognify router and the code graph load both read the route
/// stamp through it.
pub fn system_metadata_source(system_metadata: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(system_metadata?).ok()?;
    value
        .as_object()?
        .get("source")?
        .as_str()
        .map(str::to_owned)
}
