//! Shared output formatter for PDF backends.
//!
//! Both the pdfium and pure-Rust backends call [`format_pages`] so
//! their output is byte-identical and matches the Python
//! `pypdf_loader.py:70-84` format.

/// Formats per-page text into the Python-compatible output format.
///
/// Pages are 1-indexed. Empty pages and failed pages are skipped.
/// Per-page errors are logged at `warn!` level.
///
/// Output: `"Page 1:\n{text}\n\nPage 2:\n{text}\n\n..."`
pub fn format_pages(pages: &[(usize, Result<String, String>)]) -> String {
    let mut parts = Vec::new();
    for (page_num, result) in pages {
        match result {
            Ok(text) if !text.trim().is_empty() => {
                parts.push(format!("Page {page_num}:\n{text}\n"));
            }
            Ok(_) => {
                // Empty page -- skip silently
            }
            Err(e) => {
                tracing::warn!(page = page_num, error = %e, "Failed to extract text from page");
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_page() {
        let pages = vec![(1, Ok("Hello world".to_string()))];
        assert_eq!(format_pages(&pages), "Page 1:\nHello world\n");
    }

    #[test]
    fn multiple_pages() {
        let pages = vec![
            (1, Ok("First page".to_string())),
            (2, Ok("Second page".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nFirst page\n\nPage 2:\nSecond page\n"
        );
    }

    #[test]
    fn empty_pages_skipped() {
        let pages = vec![
            (1, Ok("Content".to_string())),
            (2, Ok("".to_string())),
            (3, Ok("   \n  ".to_string())),
            (4, Ok("More content".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nContent\n\nPage 4:\nMore content\n"
        );
    }

    #[test]
    fn error_pages_skipped() {
        let pages = vec![
            (1, Ok("Content".to_string())),
            (2, Err("decode error".to_string())),
            (3, Ok("After error".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nContent\n\nPage 3:\nAfter error\n"
        );
    }

    #[test]
    fn all_empty_returns_empty_string() {
        let pages: Vec<(usize, Result<String, String>)> =
            vec![(1, Ok("".to_string())), (2, Ok("  ".to_string()))];
        assert_eq!(format_pages(&pages), "");
    }

    #[test]
    fn no_pages_returns_empty_string() {
        let pages: Vec<(usize, Result<String, String>)> = vec![];
        assert_eq!(format_pages(&pages), "");
    }

    #[test]
    fn page_numbering_preserved_with_gaps() {
        // Pages 1, 3, 5 -- numbering reflects original document position
        let pages = vec![
            (1, Ok("First".to_string())),
            (3, Ok("Third".to_string())),
            (5, Ok("Fifth".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nFirst\n\nPage 3:\nThird\n\nPage 5:\nFifth\n"
        );
    }
}
