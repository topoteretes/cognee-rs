use scraper::{Html, Selector};

/// Extract plain text from HTML content
pub struct HtmlParser;

impl HtmlParser {
    /// Extract text from HTML, removing scripts, styles, and tags
    pub fn extract_text(html: &str) -> String {
        let document = Html::parse_document(html);

        // For MVP: just get visible text from common elements
        let mut text_parts = Vec::new();

        if let Ok(title_selector) = Selector::parse("title") {
            for element in document.select(&title_selector) {
                text_parts.push(element.text().collect::<String>());
            }
        }

        for tag in &["h1", "h2", "h3", "h4", "h5", "h6"] {
            if let Ok(selector) = Selector::parse(tag) {
                for element in document.select(&selector) {
                    text_parts.push(element.text().collect::<String>());
                }
            }
        }

        if let Ok(p_selector) = Selector::parse("p") {
            for element in document.select(&p_selector) {
                text_parts.push(element.text().collect::<String>());
            }
        }

        if let Ok(li_selector) = Selector::parse("li") {
            for element in document.select(&li_selector) {
                text_parts.push(element.text().collect::<String>());
            }
        }

        text_parts
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
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
