use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Data;

/// A classified document derived from a Data item.
/// Currently supports text documents; other types (PDF, image, audio) can be
/// added later by extending `classify_documents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub raw_data_location: String,
    pub mime_type: String,
    pub extension: String,
    /// Reference back to the source Data record.
    pub data_id: Uuid,
}

/// Maps Data items to Documents based on mime type.
/// Currently only text/* mime types are supported. Other types are silently
/// skipped (a warning is logged).
pub fn classify_documents(data_items: &[Data]) -> Vec<Document> {
    data_items
        .iter()
        .filter_map(|data| {
            if data.mime_type.starts_with("text/") {
                Some(Document {
                    id: data.id,
                    name: data.name.clone(),
                    raw_data_location: data.raw_data_location.clone(),
                    mime_type: data.mime_type.clone(),
                    extension: data.extension.clone(),
                    data_id: data.id,
                })
            } else {
                None
            }
        })
        .collect()
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

    #[test]
    fn classifies_text_plain() {
        let data = vec![make_data("text/plain", "txt")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].mime_type, "text/plain");
        assert_eq!(docs[0].data_id, data[0].id);
    }

    #[test]
    fn skips_non_text() {
        let data = vec![
            make_data("text/plain", "txt"),
            make_data("image/png", "png"),
            make_data("audio/mp3", "mp3"),
        ];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].extension, "txt");
    }

    #[test]
    fn classifies_text_csv() {
        let data = vec![make_data("text/csv", "csv")];
        let docs = classify_documents(&data);
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn empty_input() {
        let docs = classify_documents(&[]);
        assert!(docs.is_empty());
    }
}
