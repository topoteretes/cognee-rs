//! COGX — the Cognee eXchange format for portable memory.
//!
//! A byte-level port of Python cognee's `cognee/modules/migration/cogx.py`. An
//! archive is a directory holding `manifest.json` plus one JSONL file per
//! record kind; records are discriminated by their `kind` field. Python reads
//! one back through `COGXArchiveSource`, which is what makes a Rust-written
//! archive importable by the Python SDK.
//!
//! Only the record kinds a cognee-origin export actually emits are modelled
//! here — `entity`, `document`, `fact`, and raw nodes. The remaining kinds
//! (`episode`, `memory`, `memory_block`) exist in Python solely to carry
//! *external* providers (Mem0, Letta, Zep) into the hub format, so a Rust
//! exporter has nothing to put in them.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{MigrationError, MigrationResult};

/// Format version stamped into every manifest.
///
/// Python's reader rejects an archive whose **major** version is ahead of its
/// own (`validate_cogx_version`), so this must not be bumped independently of
/// the Python side.
pub const COGX_VERSION: &str = "0.1";

/// Manifest file name at the archive root.
pub const MANIFEST_FILE: &str = "manifest.json";
/// JSONL file holding graph nodes persisted verbatim.
pub const RAW_NODES_FILE: &str = "nodes.jsonl";
/// Social-layer file (owner + ACL grants). Never written by this crate.
pub const PERMISSIONS_FILE: &str = "permissions.json";

/// `source_system` value that unlocks id preservation on the Python importer.
///
/// `loader.py` derives `preserve_source_ids = source.source_system == "cognee"`,
/// and `COGXArchiveSource` takes that value from the manifest. Writing anything
/// else here — `"cognee-rs"` being the tempting mistake — silently downgrades
/// the import: entity ids get recomputed from names via `Entity.id_for`
/// instead of being carried across verbatim, so a Rust node and its Python
/// counterpart stop being the same node.
pub const SOURCE_SYSTEM: &str = "cognee";

/// Per-kind JSONL file names, mirroring Python's `RECORD_FILES`.
const DOCUMENTS_FILE: &str = "documents.jsonl";
const ENTITIES_FILE: &str = "entities.jsonl";
const FACTS_FILE: &str = "facts.jsonl";

/// Every file the writer owns and therefore clears on init.
const OWNED_FILES: &[&str] = &[
    DOCUMENTS_FILE,
    ENTITIES_FILE,
    FACTS_FILE,
    "episodes.jsonl",
    "memories.jsonl",
    "memory_blocks.jsonl",
    RAW_NODES_FILE,
    MANIFEST_FILE,
    PERMISSIONS_FILE,
];

/// Ownership scope of a record in the source system.
///
/// Cognee-origin exports leave every field unset; the type exists so the
/// emitted JSON carries the `"scope": {}` key Python's model expects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CogxScope {
    /// Owning user in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Owning agent in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Originating session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Originating run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// An extracted entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CogxEntity {
    /// Record discriminator; always `"entity"`.
    #[serde(default = "kind_entity")]
    pub kind: String,
    /// System that produced the record.
    pub external_system: String,
    /// The source node's id. For cognee-origin archives this is the node UUID,
    /// which the Python importer keeps verbatim.
    pub external_id: String,
    /// Ownership scope.
    #[serde(default)]
    pub scope: CogxScope,
    /// Creation time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Last-update time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// Free-form passthrough metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Entity name — the identity Python hashes when not preserving ids.
    pub name: String,
    /// Optional type name; becomes an `EntityType` node on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Alternative names, appended to the description on import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Additional typed attributes.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// Raw source content: a file, passage, or standalone text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CogxDocument {
    /// Record discriminator; always `"document"`.
    #[serde(default = "kind_document")]
    pub kind: String,
    /// System that produced the record.
    pub external_system: String,
    /// The source node's id.
    pub external_id: String,
    /// Ownership scope.
    #[serde(default)]
    pub scope: CogxScope,
    /// Creation time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Last-update time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// Free-form passthrough metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// The document text.
    pub content: String,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A triplet fact. One is emitted per graph edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CogxFact {
    /// Record discriminator; always `"fact"`.
    #[serde(default = "kind_fact")]
    pub kind: String,
    /// System that produced the record.
    pub external_system: String,
    /// Stable edge identity, `"{source}:{predicate}:{target}"`.
    pub external_id: String,
    /// Ownership scope.
    #[serde(default)]
    pub scope: CogxScope,
    /// Creation time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Last-update time in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// Free-form passthrough metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Subject: an entity `external_id`, a raw-node id, or a plain name.
    pub subject_ref: String,
    /// Relationship name.
    pub predicate: String,
    /// Object: an entity `external_id`, a raw-node id, or a plain name.
    pub object_ref: String,
    /// Natural-language rendering of the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact_text: Option<String>,
    /// Start of the fact's validity window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_at: Option<DateTime<Utc>>,
    /// End of the fact's validity window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_at: Option<DateTime<Utc>>,
    /// Extraction confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Provenance references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

fn kind_entity() -> String {
    "entity".to_string()
}
fn kind_document() -> String {
    "document".to_string()
}
fn kind_fact() -> String {
    "fact".to_string()
}

/// A typed COGX record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CogxRecord {
    /// An extracted entity.
    Entity(CogxEntity),
    /// Raw source content.
    Document(CogxDocument),
    /// A triplet fact.
    Fact(CogxFact),
}

impl CogxRecord {
    /// The record's `kind` discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Entity(_) => "entity",
            Self::Document(_) => "document",
            Self::Fact(_) => "fact",
        }
    }

    fn file_name(&self) -> &'static str {
        match self {
            Self::Entity(_) => ENTITIES_FILE,
            Self::Document(_) => DOCUMENTS_FILE,
            Self::Fact(_) => FACTS_FILE,
        }
    }
}

/// Archive manifest — `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CogxManifest {
    /// Format version. See [`COGX_VERSION`].
    pub cogx_version: String,
    /// Producing system. See [`SOURCE_SYSTEM`].
    pub source_system: String,
    /// Export timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<DateTime<Utc>>,
    /// Record counts per kind.
    #[serde(default)]
    pub counts: BTreeMap<String, usize>,
    /// Embedding model the source store used. Advisory: the archive carries no
    /// vectors, so the importing instance re-embeds with whatever it has
    /// configured, and this is the only hint that the two disagree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    /// Source store's stamped data-migration revision.
    ///
    /// Always `None` from Rust: cognee-rs does not share Python's Alembic
    /// revision line, and `None` is the defined "unknown" value that leaves
    /// the importing store's own stamp alone. Emitting a Python revision we
    /// did not actually produce would make the importer re-stamp its store
    /// backwards and replay migrations over the imported rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_revision: Option<String>,
    /// Free-form notes surfaced to whoever inspects the archive.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Parse a timestamp from an epoch number or an ISO-8601 string.
///
/// Mirrors Python's `parse_timestamp`, including its unit heuristic: cognee
/// `DataPoint`s store `created_at`/`updated_at` as epoch **milliseconds**, so a
/// raw number is scaled down by 1000 until it lands in a plausible range.
/// Values without an offset are read as UTC, which is what both stores write.
pub fn parse_timestamp(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    match value? {
        serde_json::Value::Number(number) => {
            let mut seconds = number.as_f64()?;
            if !seconds.is_finite() {
                return None;
            }
            // Python: `while abs(seconds) > 2e10: seconds /= 1000`.
            while seconds.abs() > 2e10 {
                seconds /= 1000.0;
            }
            let whole = seconds.trunc() as i64;
            let nanos = ((seconds - seconds.trunc()) * 1e9).round() as u32;
            Utc.timestamp_opt(whole, nanos.min(999_999_999)).single()
        }
        serde_json::Value::String(text) => {
            if text.is_empty() {
                return None;
            }
            if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
                return Some(parsed.with_timezone(&Utc));
            }
            // Offset-less forms are read as UTC, matching Python's
            // `fromisoformat` + `replace(tzinfo=utc)`. The space-separated
            // shape is not hypothetical: the Ladybug adapter formats every
            // node/edge timestamp as `%Y-%m-%d %H:%M:%S%.6f`
            // (`crates/graph/src/ladybug.rs`), and minute precision is
            // likewise valid ISO 8601 that `fromisoformat` accepts. Rejecting
            // either would drop the timestamp from the archive while a
            // Python-written export of the same graph kept it.
            for format in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M",
            ] {
                if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
                    return Some(Utc.from_utc_datetime(&naive));
                }
            }
            if let Some(parsed) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|naive| Utc.from_utc_datetime(&naive))
            {
                return Some(parsed);
            }
            // Losing a timestamp silently is the whole hazard here, so say so.
            tracing::debug!(value = %text, "COGX: dropping unparseable timestamp");
            None
        }
        _ => None,
    }
}

/// Streaming writer for a COGX archive directory.
///
/// One append-only handle per record kind, so peak memory stays flat no matter
/// how large the graph is. The destination is treated as owned: stale record
/// files from a previous export are removed on construction, so re-exporting
/// into the same directory never appends duplicates beside a fresh manifest.
///
/// Call [`CogxArchiveWriter::finish`] to flush and write the manifest — an
/// archive whose writer is merely dropped has no manifest and Python's
/// `find_archive_root` will reject it.
pub struct CogxArchiveWriter {
    directory: PathBuf,
    source_system: String,
    embedding_model: Option<String>,
    counts: BTreeMap<String, usize>,
    notes: Vec<String>,
    handles: BTreeMap<&'static str, BufWriter<File>>,
}

impl CogxArchiveWriter {
    /// Create (or clean) an archive directory and prepare it for writing.
    pub fn new(directory: impl Into<PathBuf>) -> MigrationResult<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory).map_err(|error| MigrationError::io(&directory, error))?;
        for file_name in OWNED_FILES {
            let path = directory.join(file_name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(MigrationError::io(path, error)),
            }
        }
        Ok(Self {
            directory,
            source_system: SOURCE_SYSTEM.to_string(),
            embedding_model: None,
            counts: BTreeMap::new(),
            notes: Vec::new(),
            handles: BTreeMap::new(),
        })
    }

    /// Record the embedding model the source store used.
    pub fn set_embedding_model(&mut self, model: Option<String>) {
        self.embedding_model = model;
    }

    /// Append a note to the manifest.
    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// The directory being written.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Write one typed record to its kind's JSONL file.
    pub fn write(&mut self, record: &CogxRecord) -> MigrationResult<()> {
        let line = serde_json::to_string(record)?;
        self.write_line(record.file_name(), &line)?;
        *self.counts.entry(record.kind().to_string()).or_insert(0) += 1;
        Ok(())
    }

    /// Persist a graph node verbatim, for full fidelity where no typed COGX
    /// mapping exists. Python rehydrates these back into `DataPoint`s, so
    /// facts pointing at them stay resolvable.
    pub fn write_raw_node(&mut self, node: &serde_json::Value) -> MigrationResult<()> {
        let line = serde_json::to_string(node)?;
        self.write_line(RAW_NODES_FILE, &line)?;
        *self.counts.entry("raw_node".to_string()).or_insert(0) += 1;
        Ok(())
    }

    fn write_line(&mut self, file_name: &'static str, line: &str) -> MigrationResult<()> {
        let directory = &self.directory;
        let handle = match self.handles.entry(file_name) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let path = directory.join(file_name);
                let file = File::create(&path).map_err(|error| MigrationError::io(path, error))?;
                entry.insert(BufWriter::new(file))
            }
        };
        writeln!(handle, "{line}")
            .map_err(|error| MigrationError::io(self.directory.join(file_name), error))
    }

    /// Flush every record file and write `manifest.json`.
    pub fn finish(mut self) -> MigrationResult<CogxManifest> {
        for (file_name, mut handle) in std::mem::take(&mut self.handles) {
            handle
                .flush()
                .map_err(|error| MigrationError::io(self.directory.join(file_name), error))?;
        }
        let manifest = CogxManifest {
            cogx_version: COGX_VERSION.to_string(),
            source_system: std::mem::take(&mut self.source_system),
            exported_at: Some(Utc::now()),
            counts: std::mem::take(&mut self.counts),
            embedding_model: self.embedding_model.take(),
            migration_revision: None,
            notes: std::mem::take(&mut self.notes),
        };
        let path = self.directory.join(MANIFEST_FILE);
        let body = serde_json::to_string_pretty(&manifest)?;
        fs::write(&path, body).map_err(|error| MigrationError::io(path, error))?;
        Ok(manifest)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn epoch_milliseconds_scale_down_to_seconds() {
        // DataPoint.created_at is epoch-ms; 1_768_164_683_000 must not be read
        // as seconds (which would land in the year 58000).
        let parsed = parse_timestamp(Some(&json!(1_768_164_683_000_i64))).unwrap();
        assert_eq!(parsed.timestamp(), 1_768_164_683);
    }

    #[test]
    fn epoch_seconds_pass_through_unscaled() {
        let parsed = parse_timestamp(Some(&json!(1_768_164_683_i64))).unwrap();
        assert_eq!(parsed.timestamp(), 1_768_164_683);
    }

    #[test]
    fn rfc3339_and_offsetless_strings_both_parse_as_utc() {
        let with_offset = parse_timestamp(Some(&json!("2026-01-11T20:51:23+00:00"))).unwrap();
        let without_offset = parse_timestamp(Some(&json!("2026-01-11T20:51:23"))).unwrap();
        assert_eq!(with_offset, without_offset);
        assert_eq!(with_offset.timestamp(), 1_768_164_683);
    }

    #[test]
    fn ladybug_space_separated_timestamps_are_accepted() {
        // The Ladybug adapter writes `%Y-%m-%d %H:%M:%S%.6f`; rejecting that
        // shape dropped created_at/valid_at from the archive silently.
        let parsed = parse_timestamp(Some(&json!("2026-01-11 20:51:23.000000"))).unwrap();
        assert_eq!(parsed.timestamp(), 1_768_164_683);
    }

    #[test]
    fn minute_precision_iso_strings_are_accepted() {
        // Valid ISO 8601, and Python's fromisoformat takes it.
        let parsed = parse_timestamp(Some(&json!("2026-01-11T20:51"))).unwrap();
        assert_eq!(parsed.timestamp(), 1_768_164_660);
    }

    #[test]
    fn bare_dates_are_read_as_utc_midnight() {
        let parsed = parse_timestamp(Some(&json!("2026-01-11"))).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-01-11T00:00:00+00:00");
    }

    #[test]
    fn unparseable_values_are_dropped_rather_than_guessed() {
        for value in [json!(null), json!(true), json!("not a date"), json!([])] {
            assert!(
                parse_timestamp(Some(&value)).is_none(),
                "unexpectedly parsed {value}"
            );
        }
        assert!(parse_timestamp(None).is_none());
    }

    #[test]
    fn writer_clears_stale_records_so_reexport_does_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive");

        let mut writer = CogxArchiveWriter::new(&path).unwrap();
        writer
            .write(&CogxRecord::Entity(sample_entity("alice")))
            .unwrap();
        writer.finish().unwrap();

        let mut writer = CogxArchiveWriter::new(&path).unwrap();
        writer
            .write(&CogxRecord::Entity(sample_entity("bob")))
            .unwrap();
        let manifest = writer.finish().unwrap();

        let entities = fs::read_to_string(path.join(ENTITIES_FILE)).unwrap();
        assert_eq!(
            entities.lines().count(),
            1,
            "stale record survived re-export"
        );
        assert!(entities.contains("bob"));
        assert_eq!(manifest.counts.get("entity"), Some(&1));
    }

    #[test]
    fn manifest_names_cognee_as_the_source_system() {
        // The Python importer keys id preservation off this exact string.
        let dir = tempfile::tempdir().unwrap();
        let writer = CogxArchiveWriter::new(dir.path().join("archive")).unwrap();
        let manifest = writer.finish().unwrap();
        assert_eq!(manifest.source_system, "cognee");
        assert_eq!(manifest.cogx_version, "0.1");
        assert!(
            manifest.migration_revision.is_none(),
            "Rust must not claim a Python Alembic revision"
        );
    }

    #[test]
    fn optional_fields_are_omitted_rather_than_written_as_null() {
        // Python writes with `exclude_none=True`; a literal `null` would still
        // parse, but keeping parity makes archives diffable across SDKs.
        let record = CogxRecord::Entity(sample_entity("alice"));
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("null"), "{json}");
        assert!(json.contains("\"kind\":\"entity\""));
        assert!(json.contains("\"scope\":{}"));
    }

    fn sample_entity(name: &str) -> CogxEntity {
        CogxEntity {
            kind: kind_entity(),
            external_system: SOURCE_SYSTEM.to_string(),
            external_id: format!("id-{name}"),
            scope: CogxScope::default(),
            created_at: None,
            updated_at: None,
            metadata: serde_json::Map::new(),
            name: name.to_string(),
            entity_type: None,
            description: None,
            aliases: Vec::new(),
            attributes: serde_json::Map::new(),
        }
    }
}
