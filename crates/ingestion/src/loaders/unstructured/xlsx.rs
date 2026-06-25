//! XLSX/XLS/ODS spreadsheet text extraction via `calamine`.
//!
//! Iterates all sheets and rows, formatting cell values as
//! comma-separated strings. Non-empty rows are joined with `"\n\n"`
//! to match the Python unstructured output format.

use std::io::Cursor;

use calamine::{Data, Reader, open_workbook_auto_from_rs};

use super::super::LoaderError;

/// Extract text from an XLSX, XLS, or ODS spreadsheet.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut workbook = open_workbook_auto_from_rs(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open spreadsheet: {e}")))?;

    let sheet_names: Vec<String> = workbook.sheet_names().to_vec();
    let mut elements: Vec<String> = Vec::new();

    for name in &sheet_names {
        let range = workbook.worksheet_range(name).map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to read sheet '{name}': {e}"))
        })?;

        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .map(|cell| match cell {
                    Data::Empty => String::new(),
                    Data::String(s) => s.clone(),
                    Data::Int(i) => i.to_string(),
                    Data::Float(f) => f.to_string(),
                    Data::Bool(b) => b.to_string(),
                    Data::Error(e) => format!("#ERR:{e:?}"),
                    Data::DateTime(dt) => dt.to_string(),
                    Data::DateTimeIso(s) => s.clone(),
                    Data::DurationIso(s) => s.clone(),
                })
                .collect();

            let row_text = cells.join(", ").trim().to_string();
            if !row_text.is_empty() {
                elements.push(row_text);
            }
        }
    }

    Ok(elements.join("\n\n"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn invalid_bytes_returns_error() {
        let result = extract(b"not a valid spreadsheet");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, LoaderError::ExtractionFailed(_)),
            "expected ExtractionFailed, got {err:?}"
        );
    }

    #[test]
    fn empty_bytes_returns_error() {
        let result = extract(b"");
        assert!(result.is_err());
    }
}
