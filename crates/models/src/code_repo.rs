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
