use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::Data;
use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// A classified document derived from a Data item.
///
/// Mirrors the Python `Document` class hierarchy. In Python, each document type
/// is a separate class (TextDocument, PdfDocument, etc.). In Rust we use a single
/// struct with a `document_type` field and the `base.data_type` discriminator
/// set to the class name (e.g. "TextDocument", "PdfDocument").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// DataPoint base — carries id, timestamps, metadata, data_type discriminator.
    #[serde(flatten)]
    pub base: DataPoint,
    /// Document type category: "text", "pdf", "csv", "image", "audio", "unstructured", "dlt_row".
    pub document_type: String,
    pub name: String,
    pub raw_data_location: String,
    pub mime_type: String,
    pub extension: String,
    /// Reference back to the source Data record.
    pub data_id: Uuid,
    /// Pretty-printed external metadata JSON, if any.
    pub external_metadata: Option<String>,
}

/// Map a file extension to a document type string.
///
/// Matches the 39-entry `EXTENSION_TO_DOCUMENT_CLASS` mapping in the Python SDK
/// (`cognee/tasks/documents/classify_documents.py`).
fn extension_to_doc_type(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "pdf" => Some("pdf"),
        "txt" => Some("text"),
        "csv" => Some("csv"),
        "docx" | "doc" | "odt" | "xls" | "xlsx" | "ppt" | "pptx" | "odp" | "ods" => {
            Some("unstructured")
        }
        "png" | "dwg" | "xcf" | "jpg" | "jpx" | "apng" | "gif" | "webp" | "cr2" | "tif" | "bmp"
        | "jxr" | "psd" | "ico" | "heic" | "avif" => Some("image"),
        "aac" | "mid" | "mp3" | "m4a" | "ogg" | "flac" | "wav" | "amr" | "aiff" => Some("audio"),
        _ => None,
    }
}

/// Return the `data_type` discriminator (Python class name) for a document type.
fn doc_type_to_class_name(doc_type: &str) -> &'static str {
    match doc_type {
        "text" => "TextDocument",
        "pdf" => "PdfDocument",
        "csv" => "CsvDocument",
        "image" => "ImageDocument",
        "audio" => "AudioDocument",
        "unstructured" => "UnstructuredDocument",
        "dlt_row" => "DltRowDocument",
        _ => "Document",
    }
}

/// Check whether the `external_metadata` JSON indicates a DLT source.
///
/// Mirrors Python `cognee/tasks/ingestion/dlt_utils.py:is_dlt_sourced`.
fn is_dlt_sourced(external_metadata: &Option<String>) -> bool {
    external_metadata
        .as_ref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get("source")?.as_str().map(|s| s == "dlt"))
        .unwrap_or(false)
}

/// Classify Data items into Documents based on file extension.
///
/// Mirrors the Python `classify_documents` function. DLT-sourced items are
/// classified as `DltRowDocument`; all others use the extension-to-document-type
/// mapping. Items with unrecognised extensions are silently skipped.
pub fn classify_documents(data_items: &[Data]) -> Vec<Document> {
    data_items
        .iter()
        .filter_map(|data| {
            // DLT detection takes priority
            let doc_type = if is_dlt_sourced(&data.external_metadata) {
                "dlt_row"
            } else {
                extension_to_doc_type(&data.extension)?
            };

            let class_name = doc_type_to_class_name(doc_type);
            let mut base = DataPoint::new(class_name, None);
            base.id = data.id; // use Data's deterministic ID
            base.set_metadata("index_fields", json!(["name"]));

            // Format external_metadata as indented JSON (Python does json.dumps(..., indent=4))
            let formatted_metadata = data.external_metadata.as_ref().and_then(|m| {
                let v: serde_json::Value = serde_json::from_str(m).ok()?;
                serde_json::to_string_pretty(&v).ok()
            });

            let mut doc = Document {
                base,
                document_type: doc_type.to_string(),
                name: data.name.clone(),
                raw_data_location: data.raw_data_location.clone(),
                mime_type: data.mime_type.clone(),
                extension: data.extension.clone(),
                data_id: data.id,
                external_metadata: formatted_metadata.or(data.external_metadata.clone()),
            };

            // update_node_set: parse external_metadata for node_set array
            // Mirrors Python cognee/tasks/documents/classify_documents.py:update_node_set()
            if let Some(ref meta_str) = doc.external_metadata
                && let Ok(meta_val) = serde_json::from_str::<serde_json::Value>(meta_str)
                && let Some(node_set_array) = meta_val.get("node_set").and_then(|v| v.as_array())
            {
                // Build NodeSet-like JSON values with deterministic IDs
                // Python: NodeSet(id=generate_node_id(f"NodeSet:{name}"), name=name)
                let node_set_values: Vec<serde_json::Value> = node_set_array
                    .iter()
                    .filter_map(|v| {
                        let name = v.as_str()?;
                        let key = format!("NodeSet:{}", name)
                            .to_lowercase()
                            .replace(' ', "_")
                            .replace('\'', "");
                        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key.as_bytes());
                        Some(json!({
                            "id": id.to_string(),
                            "name": name,
                            "type": "NodeSet"
                        }))
                    })
                    .collect();

                if !node_set_values.is_empty() {
                    // source_node_set = comma-separated names (Python: ", ".join(node_set))
                    let names: Vec<&str> =
                        node_set_array.iter().filter_map(|v| v.as_str()).collect();
                    doc.base.source_node_set = Some(names.join(", "));
                    doc.base.belongs_to_set = Some(node_set_values);
                }
            }

            Some(doc)
        })
        .collect()
}

impl HasDataPoint for Document {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — Document has no nested
    // DataPoint-bearing fields (links to its source `Data` by `data_id: Uuid`).
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_data(mime_type: &str, extension: &str) -> Data {
        Data::builder(
            Uuid::new_v4(),
            format!("test.{extension}"),
            "/storage/test",
            "text://test",
            extension,
            mime_type,
            "hash123",
            Uuid::new_v4(),
        )
        .build()
    }

    fn make_data_with_metadata(mime_type: &str, extension: &str, metadata: &str) -> Data {
        Data::builder(
            Uuid::new_v4(),
            format!("test.{extension}"),
            "/storage/test",
            "text://test",
            extension,
            mime_type,
            "hash123",
            Uuid::new_v4(),
        )
        .external_metadata(metadata)
        .build()
    }

    // ----- Extension-based classification tests -----

    #[test]
    fn classifies_text_plain() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "text");
        assert_eq!(docs[0].base.data_type, "TextDocument");
        assert_eq!(docs[0].mime_type, "text/plain");
        assert_eq!(docs[0].data_id, data[0].id);
        assert_eq!(docs[0].base.id, data[0].id);
        assert_eq!(docs[0].base.data_type, "TextDocument");
        assert_eq!(
            docs[0].base.get_metadata("index_fields"),
            Some(&serde_json::json!(["name"]))
        );
    }

    #[test]
    fn classifies_pdf() {
        let data = vec![make_data("application/pdf", "pdf")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "pdf");
        assert_eq!(docs[0].base.data_type, "PdfDocument");
    }

    #[test]
    fn classifies_csv() {
        let data = vec![make_data("text/csv", "csv")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "csv");
        assert_eq!(docs[0].base.data_type, "CsvDocument");
    }

    #[test]
    fn classifies_image_extensions() {
        for ext in &[
            "png", "dwg", "xcf", "jpg", "jpx", "apng", "gif", "webp", "cr2", "tif", "bmp", "jxr",
            "psd", "ico", "heic", "avif",
        ] {
            let data = vec![make_data(&format!("image/{ext}"), ext)];
            let docs = classify_documents(&data);
            assert_eq!(docs.len(), 1, "failed for extension: {ext}");
            assert_eq!(
                docs[0].document_type, "image",
                "failed for extension: {ext}"
            );
            assert_eq!(
                docs[0].base.data_type, "ImageDocument",
                "failed for extension: {ext}"
            );
        }
    }

    #[test]
    fn classifies_audio_extensions() {
        for ext in &[
            "aac", "mid", "mp3", "m4a", "ogg", "flac", "wav", "amr", "aiff",
        ] {
            let data = vec![make_data(&format!("audio/{ext}"), ext)];
            let docs = classify_documents(&data);
            assert_eq!(docs.len(), 1, "failed for extension: {ext}");
            assert_eq!(
                docs[0].document_type, "audio",
                "failed for extension: {ext}"
            );
            assert_eq!(
                docs[0].base.data_type, "AudioDocument",
                "failed for extension: {ext}"
            );
        }
    }

    #[test]
    fn classifies_unstructured_extensions() {
        for ext in &[
            "docx", "doc", "odt", "xls", "xlsx", "ppt", "pptx", "odp", "ods",
        ] {
            let data = vec![make_data("application/octet-stream", ext)];
            let docs = classify_documents(&data);
            assert_eq!(docs.len(), 1, "failed for extension: {ext}");
            assert_eq!(
                docs[0].document_type, "unstructured",
                "failed for extension: {ext}"
            );
            assert_eq!(
                docs[0].base.data_type, "UnstructuredDocument",
                "failed for extension: {ext}"
            );
        }
    }

    // ----- Unknown extensions are skipped -----

    #[test]
    fn skips_unknown_extensions() {
        let data = vec![make_data("application/octet-stream", "xyz")];
        let docs = classify_documents(&data);
        assert!(docs.is_empty());
    }

    #[test]
    fn source_code_extensions_are_not_classified() {
        for ext in &["py", "rs", "js", "ts", "c", "cpp", "go", "java", "rb", "sh"] {
            let data = vec![make_data("text/plain", ext)];
            let docs = classify_documents(&data);
            assert!(docs.is_empty(), "extension .{ext} should not be classified");
        }
    }

    // ----- Mixed input: only known extensions pass through -----

    #[test]
    fn mixed_input_filters_correctly() {
        let data = vec![
            make_data("text/plain", "txt"),
            make_data("application/octet-stream", "xyz"),
            make_data("application/pdf", "pdf"),
            make_data("image/png", "png"),
            make_data("audio/mp3", "mp3"),
        ];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 4);
        assert_eq!(docs[0].document_type, "text");
        assert_eq!(docs[1].document_type, "pdf");
        assert_eq!(docs[2].document_type, "image");
        assert_eq!(docs[3].document_type, "audio");
    }

    // ----- DLT detection -----

    #[test]
    fn classifies_dlt_sourced_data() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"source": "dlt"}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "dlt_row");
        assert_eq!(docs[0].base.data_type, "DltRowDocument");
    }

    #[test]
    fn dlt_detection_with_unknown_extension() {
        // DLT sourced items should be classified even with unknown extensions
        let data = vec![make_data_with_metadata(
            "application/octet-stream",
            "xyz",
            r#"{"source": "dlt"}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "dlt_row");
    }

    #[test]
    fn non_dlt_metadata_does_not_affect_classification() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"source": "other"}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].document_type, "text");
    }

    // ----- External metadata formatting -----

    #[test]
    fn formats_external_metadata_as_pretty_json() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"key":"value","nested":{"a":1}}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        let meta = docs[0].external_metadata.as_ref().unwrap();
        // Pretty-printed JSON should contain newlines and indentation
        assert!(meta.contains('\n'));
        assert!(meta.contains("  "));
    }

    #[test]
    fn invalid_json_metadata_passed_through_as_is() {
        let data = vec![make_data_with_metadata("text/plain", "txt", "not-json")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        // Invalid JSON can't be pretty-printed, so original is kept
        assert_eq!(docs[0].external_metadata.as_ref().unwrap(), "not-json");
    }

    // ----- DataPoint base -----

    #[test]
    fn document_has_index_fields_metadata() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(
            docs[0].base.get_metadata("index_fields"),
            Some(&json!(["name"]))
        );
    }

    #[test]
    fn document_id_matches_data_id() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(docs[0].base.id, data[0].id);
        assert_eq!(docs[0].data_id, data[0].id);
    }

    // ----- Empty input -----

    #[test]
    fn empty_input() {
        let docs = classify_documents(&[]);
        assert!(docs.is_empty());
    }

    // ----- Case insensitivity -----

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(extension_to_doc_type("PDF"), Some("pdf"));
        assert_eq!(extension_to_doc_type("Txt"), Some("text"));
        assert_eq!(extension_to_doc_type("PNG"), Some("image"));
        assert_eq!(extension_to_doc_type("MP3"), Some("audio"));
    }

    // ----- NodeSet handling (update_node_set) -----

    #[test]
    fn node_set_populates_belongs_to_set_and_source_node_set() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"node_set": ["setA", "setB"]}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);

        // belongs_to_set should have two NodeSet entries
        let bts = docs[0].base.belongs_to_set.as_ref().unwrap();
        assert_eq!(bts.len(), 2);

        // Each entry should have id, name, type
        assert_eq!(bts[0]["name"], "setA");
        assert_eq!(bts[0]["type"], "NodeSet");
        assert_eq!(bts[1]["name"], "setB");
        assert_eq!(bts[1]["type"], "NodeSet");

        // IDs should be deterministic UUID5
        let key_a = "nodeset:seta"; // lowercased, spaces→underscores
        let expected_id_a =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key_a.as_bytes()).to_string();
        assert_eq!(bts[0]["id"], expected_id_a);

        // source_node_set should be comma-separated names
        assert_eq!(docs[0].base.source_node_set.as_ref().unwrap(), "setA, setB");
    }

    #[test]
    fn node_set_single_entry() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"node_set": ["only_one"]}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);

        let bts = docs[0].base.belongs_to_set.as_ref().unwrap();
        assert_eq!(bts.len(), 1);
        assert_eq!(bts[0]["name"], "only_one");

        assert_eq!(docs[0].base.source_node_set.as_ref().unwrap(), "only_one");
    }

    #[test]
    fn no_node_set_key_leaves_belongs_to_set_unset() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"other_key": "value"}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert!(docs[0].base.belongs_to_set.is_none());
        assert!(docs[0].base.source_node_set.is_none());
    }

    #[test]
    fn node_set_not_array_leaves_belongs_to_set_unset() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"node_set": "not_an_array"}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert!(docs[0].base.belongs_to_set.is_none());
        assert!(docs[0].base.source_node_set.is_none());
    }

    #[test]
    fn node_set_empty_array_leaves_belongs_to_set_unset() {
        let data = vec![make_data_with_metadata(
            "text/plain",
            "txt",
            r#"{"node_set": []}"#,
        )];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        // Empty array produces no NodeSet values, so stays unset
        assert!(docs[0].base.belongs_to_set.is_none());
        assert!(docs[0].base.source_node_set.is_none());
    }

    #[test]
    fn node_set_with_no_metadata_leaves_belongs_to_set_unset() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert!(docs[0].base.belongs_to_set.is_none());
        assert!(docs[0].base.source_node_set.is_none());
    }

    #[test]
    fn document_implements_has_datapoint() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        let dp_id = docs[0].base.id;
        assert_eq!(docs[0].data_point().id, dp_id);
        let mut doc = docs[0].clone();
        assert_eq!(doc.data_point_mut().id, dp_id);
    }
}
