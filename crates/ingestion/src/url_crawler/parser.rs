use scraper::{Html, Selector};

/// Extract plain text from HTML content
pub struct HtmlParser;

impl HtmlParser {
    /// Extract text from HTML using the full 39-rule engine ported from Python's
    /// BeautifulSoupLoader. Strips `<script>`, `<style>`, `<noscript>`, and HTML
    /// comments before applying extraction rules.
    pub fn extract_text(html: &str) -> String {
        super::html_rules::extract_html(html)
    }

    /// Extract title from HTML
    pub fn extract_title(html: &str) -> Option<String> {
        let document = Html::parse_document(html);
        let title_selector = Selector::parse("title").ok()?;

        document
            .select(&title_selector)
            .next()
            .map(|element| element.text().collect::<String>().trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_basic() {
        let html = r#"
            <html>
                <head><title>Test Page</title></head>
                <body>
                    <h1>Hello World</h1>
                    <p>This is a paragraph.</p>
                    <p>Another paragraph.</p>
                </body>
            </html>
        "#;

        let text = HtmlParser::extract_text(html);
        assert!(text.contains("Test Page"));
        assert!(text.contains("Hello World"));
        assert!(text.contains("This is a paragraph"));
    }

    #[test]
    fn test_extract_title() {
        let html = r#"<html><head><title>My Title</title></head></html>"#;
        let title = HtmlParser::extract_title(html);
        assert_eq!(title, Some("My Title".to_string()));
    }
}
