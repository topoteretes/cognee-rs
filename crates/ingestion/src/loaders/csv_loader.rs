use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Loader for CSV documents.
///
/// Parses the input bytes as UTF-8 CSV with headers. Each data row is
/// formatted as `"col1: val1, col2: val2, ..."` and returned as
/// [`LoaderOutput::Rows`], matching Python's `CsvDocument.py:24-25`.
pub struct CsvLoader;

#[async_trait]
impl DocumentLoader for CsvLoader {
    fn engine_name(&self) -> &'static str {
        "csv_loader"
    }

    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|e| LoaderError::InvalidUtf8(e.to_string()))?;

        let mut reader = csv::Reader::from_reader(text.as_bytes());

        let headers: Vec<String> = reader
            .headers()
            .map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?
            .iter()
            .map(|h| h.to_string())
            .collect();

        if headers.is_empty() {
            return Ok(LoaderOutput::Rows(Vec::new()));
        }

        let mut rows = Vec::new();
        for result in reader.records() {
            let record = result.map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?;
            let pairs: Vec<String> = headers
                .iter()
                .zip(record.iter())
                .map(|(col, val)| format!("{col}: {val}"))
                .collect();
            rows.push(pairs.join(", "));
        }

        Ok(LoaderOutput::Rows(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_models::DataPoint;

    fn test_doc() -> Document {
        Document {
            base: DataPoint::new("CsvDocument", None),
            document_type: "csv".to_string(),
            name: "test.csv".to_string(),
            raw_data_location: "file:///tmp/test.csv".to_string(),
            mime_type: "text/csv".to_string(),
            extension: "csv".to_string(),
            data_id: uuid::Uuid::new_v4(),
            external_metadata: None,
        }
    }

    #[tokio::test]
    async fn basic_csv() {
        let csv_data = b"name,age,city\nAlice,30,NYC\nBob,25,LA\n";
        let loader = CsvLoader;
        let result = loader
            .extract(csv_data, &test_doc())
            .await
            .expect("should succeed");
        match result {
            LoaderOutput::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], "name: Alice, age: 30, city: NYC");
                assert_eq!(rows[1], "name: Bob, age: 25, city: LA");
            }
            other => panic!("expected Rows, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn headers_only() {
        let csv_data = b"name,age,city\n";
        let loader = CsvLoader;
        let result = loader
            .extract(csv_data, &test_doc())
            .await
            .expect("should succeed");
        match result {
            LoaderOutput::Rows(rows) => {
                assert!(rows.is_empty(), "no data rows expected");
            }
            other => panic!("expected Rows, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn special_characters() {
        let csv_data = b"col\nvalue with \"quotes\"\n\"commas, here\"\n";
        let loader = CsvLoader;
        let result = loader
            .extract(csv_data, &test_doc())
            .await
            .expect("should succeed");
        match result {
            LoaderOutput::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], "col: value with \"quotes\"");
                assert_eq!(rows[1], "col: commas, here");
            }
            other => panic!("expected Rows, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_utf8() {
        let loader = CsvLoader;
        let result = loader.extract(&[0xFF, 0xFE, 0x00], &test_doc()).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, LoaderError::InvalidUtf8(_)),
            "expected InvalidUtf8 error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn empty_input() {
        let loader = CsvLoader;
        let result = loader
            .extract(b"", &test_doc())
            .await
            .expect("should succeed");
        match result {
            LoaderOutput::Rows(rows) => {
                assert!(rows.is_empty());
            }
            other => panic!("expected Rows, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn engine_name_matches() {
        assert_eq!(CsvLoader.engine_name(), "csv_loader");
    }
}
