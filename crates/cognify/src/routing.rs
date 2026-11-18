//! Cognify routing policy: which task route a data item takes.
//!
//! Port of Python's `cognee/modules/cognify/routing.py` together with the
//! route half of `classify_documents.document_class_for` and the shared
//! `system_metadata["source"]` reader (`dlt_utils.metadata_source`). One pure,
//! synchronous function — no I/O, no configuration.
//!
//! The routing fact is written onto each [`Data`] record at add time
//! (`Data.system_metadata`); this module only reads it. The user-writable
//! `external_metadata` is never consulted, so users cannot steer a record into
//! (or out of) a route.
//!
//! Nothing routes on this yet: cognify still runs every item down the
//! standard pipeline. Later SDK-1053 stages split cognify by route.

use cognee_models::Data;
use uuid::Uuid;

/// The `system_metadata.source` stamp of a DLT source manifest.
pub const SOURCE_DLT_SOURCE: &str = "dlt_source";
/// The `system_metadata.source` stamp of a pre-manifest per-row DLT record.
pub const SOURCE_DLT_LEGACY: &str = "dlt";
/// The `system_metadata.source` stamp of a single code file.
pub const SOURCE_CODE: &str = "code";
/// The `system_metadata.source` stamp of a code-repository manifest.
pub const SOURCE_CODE_REPO: &str = "code_repo";

/// The task routes cognify can send a data item down (Python `CognifyRoute`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CognifyRoute {
    /// The default chunk → extract → summarize pipeline.
    Standard,
    /// A DLT source manifest (`system_metadata.source == "dlt_source"`).
    ///
    /// Declared for parity with Python. Rust's own DLT ingestion still stamps
    /// `external_metadata.source` rather than `system_metadata` and runs down
    /// [`Standard`](Self::Standard) (SDK-1053 decision 2), so only a manifest
    /// row Python wrote to a shared database classifies here.
    DltSource,
    /// A single code file (`system_metadata.source == "code"`).
    Code,
    /// A code-repository manifest (`system_metadata.source == "code_repo"`).
    CodeRepo,
}

impl CognifyRoute {
    /// The route's wire name, as Python's enum values spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::DltSource => "dlt_source",
            Self::Code => "code",
            Self::CodeRepo => "code_repo",
        }
    }
}

impl std::fmt::Display for CognifyRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A data item no route accepts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CognifyRouteError {
    /// A pre-manifest per-row DLT record (`system_metadata.source == "dlt"`).
    /// Python refuses it rather than sending structured rows to the LLM
    /// (`classify_documents.document_class_for`); the message is Python's.
    #[error(
        "Data item {data_id} is a pre-manifest per-row DLT record \
         (system_metadata.source == 'dlt'), which is no longer supported. \
         Delete it or re-add the DLT source to ingest it as a manifest."
    )]
    LegacyDltRecord {
        /// The refused row.
        data_id: Uuid,
    },
}

/// The `source` of a `system_metadata` JSON text, or `None` when there is no
/// metadata, it is not a JSON object, or its `source` is not a string
/// (Python `metadata_source`). A Python-written `null` reads as none.
pub fn metadata_source(system_metadata: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(system_metadata?).ok()?;
    value
        .as_object()?
        .get("source")?
        .as_str()
        .map(str::to_owned)
}

/// The cognify route for one data item: a pure function of the record's
/// `system_metadata.source` (Python `cognify_route_for`).
///
/// `"dlt_source"` → [`CognifyRoute::DltSource`], `"code"` →
/// [`CognifyRoute::Code`], `"code_repo"` → [`CognifyRoute::CodeRepo`],
/// anything else (including no stamp) → [`CognifyRoute::Standard`]. A legacy
/// `"dlt"` stamp is refused with [`CognifyRouteError::LegacyDltRecord`], as
/// Python raises for it.
pub fn cognify_route_for(data: &Data) -> Result<CognifyRoute, CognifyRouteError> {
    match metadata_source(data.system_metadata.as_deref()).as_deref() {
        Some(SOURCE_DLT_SOURCE) => Ok(CognifyRoute::DltSource),
        Some(SOURCE_DLT_LEGACY) => Err(CognifyRouteError::LegacyDltRecord { data_id: data.id }),
        Some(SOURCE_CODE) => Ok(CognifyRoute::Code),
        Some(SOURCE_CODE_REPO) => Ok(CognifyRoute::CodeRepo),
        _ => Ok(CognifyRoute::Standard),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn data(system_metadata: Option<&str>, external_metadata: Option<&str>) -> Data {
        let mut builder = Data::builder(
            Uuid::new_v4(),
            "n",
            "file:///r",
            "/o",
            "txt",
            "text/plain",
            "h",
            Uuid::new_v4(),
        );
        if let Some(meta) = system_metadata {
            builder = builder.system_metadata(meta);
        }
        if let Some(meta) = external_metadata {
            builder = builder.external_metadata(meta);
        }
        builder.build()
    }

    #[test]
    fn routes_by_system_metadata_source() {
        let cases: &[(Option<&str>, CognifyRoute)] = &[
            (None, CognifyRoute::Standard),
            (Some("null"), CognifyRoute::Standard),
            (Some("{}"), CognifyRoute::Standard),
            (Some("[]"), CognifyRoute::Standard),
            (Some("not json"), CognifyRoute::Standard),
            (Some(r#"{"source": 1}"#), CognifyRoute::Standard),
            (Some(r#"{"source": null}"#), CognifyRoute::Standard),
            (Some(r#"{"source": "web"}"#), CognifyRoute::Standard),
            (Some(r#"{"source": "CODE"}"#), CognifyRoute::Standard),
            (Some(r#"{"source": "dlt_source"}"#), CognifyRoute::DltSource),
            (Some(r#"{"source": "code"}"#), CognifyRoute::Code),
            (
                Some(r#"{"source": "code_repo", "repo_path": "/r"}"#),
                CognifyRoute::CodeRepo,
            ),
            // Python's json.dumps spacing and key order do not matter.
            (
                Some(r#"{"x":1,"source":"code_repo"}"#),
                CognifyRoute::CodeRepo,
            ),
        ];
        for (meta, expected) in cases {
            let route = cognify_route_for(&data(*meta, None)).expect("routable");
            assert_eq!(route, *expected, "system_metadata {meta:?}");
        }
    }

    #[test]
    fn user_external_metadata_never_routes() {
        for source in ["code", "code_repo", "dlt_source", "dlt"] {
            let meta = format!(r#"{{"source": "{source}"}}"#);
            let route = cognify_route_for(&data(None, Some(&meta))).expect("routable");
            assert_eq!(route, CognifyRoute::Standard, "external source {source}");
        }
    }

    #[test]
    fn legacy_dlt_record_is_refused() {
        let item = data(Some(r#"{"source": "dlt"}"#), None);
        let error = cognify_route_for(&item).expect_err("legacy DLT is refused");
        assert_eq!(
            error,
            CognifyRouteError::LegacyDltRecord { data_id: item.id }
        );
        assert!(
            error
                .to_string()
                .contains("pre-manifest per-row DLT record")
        );
    }

    #[test]
    fn route_names_match_python() {
        assert_eq!(CognifyRoute::Standard.to_string(), "standard");
        assert_eq!(CognifyRoute::DltSource.to_string(), "dlt_source");
        assert_eq!(CognifyRoute::Code.to_string(), "code");
        assert_eq!(CognifyRoute::CodeRepo.to_string(), "code_repo");
    }
}
