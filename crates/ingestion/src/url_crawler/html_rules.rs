//! Rule-driven HTML text extractor ported from Python's BeautifulSoupLoader.
//!
//! Embeds `html_rules.toml` at compile time and parses it once via `LazyLock`.
//! The 39 extraction rules match Python's `_get_default_extraction_rules()` from
//! `cognee/infrastructure/loaders/external/beautiful_soup_loader.py`.

use std::sync::LazyLock;

use scraper::{Html, Node, Selector};
use serde::Deserialize;

/// A single HTML extraction rule as deserialized from TOML.
#[derive(Debug, Deserialize)]
struct HtmlRule {
    /// Human-readable identifier matching the Python dict key.
    #[allow(dead_code)]
    name: String,

    /// CSS Level 3 selector string.
    selector: String,

    /// If set, extract this HTML attribute instead of text content.
    #[serde(default)]
    attr: Option<String>,

    /// `true` = select all matching elements; `false` = first match only.
    #[serde(default)]
    all: bool,

    /// Separator for joining multiple matches.
    #[serde(default = "default_join_with")]
    join_with: String,
}

fn default_join_with() -> String {
    " ".to_string()
}

/// Wrapper for TOML deserialization of the rule array.
#[derive(Debug, Deserialize)]
struct HtmlRules {
    rule: Vec<HtmlRule>,
}

/// A parsed rule that pairs the deserialized `HtmlRule` with a pre-compiled
/// `scraper::Selector` to avoid re-parsing on every `extract_html` call.
struct ParsedRule {
    rule: HtmlRule,
    selector: Selector,
}

/// Embedded TOML rules, parsed once on first access.
static PARSED_RULES: LazyLock<Vec<ParsedRule>> = LazyLock::new(|| {
    let toml_str = include_str!("html_rules.toml");
    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
    let rules: HtmlRules = toml::from_str(toml_str)
        .expect("html_rules.toml is embedded at compile time and must be valid TOML");

    rules
        .rule
        .into_iter()
        .map(|rule| {
            let selector = Selector::parse(&rule.selector).unwrap_or_else(|e| {
                panic!(
                    "CSS selector {:?} for rule {:?} failed to parse: {e:?}",
                    rule.selector, rule.name
                )
            });
            ParsedRule { rule, selector }
        })
        .collect()
});

/// Remove `<script>`, `<style>`, `<noscript>` elements and HTML comment nodes
/// from the parsed document tree so they do not contribute text to any rule.
fn strip_unwanted_nodes(html: &mut Html) {
    let tags_to_strip = ["script", "style", "noscript"];

    // Pass 1: collect NodeIds of nodes to remove.
    let ids_to_remove: Vec<_> = html
        .tree
        .nodes()
        .filter_map(|node_ref| {
            let val = node_ref.value();
            match val {
                Node::Element(el) if tags_to_strip.contains(&el.name()) => Some(node_ref.id()),
                Node::Comment(_) => Some(node_ref.id()),
                _ => None,
            }
        })
        .collect();

    // Pass 2: detach each collected node (and its children) from the tree.
    // IDs remain valid because detach only unlinks, it does not deallocate.
    for id in ids_to_remove {
        if let Some(mut node) = html.tree.get_mut(id) {
            node.detach();
        }
    }
}

/// Extract text from a single element, either from an attribute or from
/// descendant text nodes.
fn extract_text_from_element(el: &scraper::ElementRef, attr: Option<&str>) -> String {
    match attr {
        Some(attr_name) => el.value().attr(attr_name).unwrap_or("").trim().to_string(),
        None => el.text().collect::<Vec<_>>().join(" ").trim().to_string(),
    }
}

/// Extract text from HTML using the full 39-rule set ported from Python's
/// BeautifulSoupLoader.
///
/// The algorithm:
/// 1. Parse the raw HTML into a document tree.
/// 2. Strip `<script>`, `<style>`, `<noscript>`, and HTML comment nodes.
/// 3. For each rule, select matching elements and extract text or attribute values.
/// 4. Join all non-empty rule outputs with `" "` and trim (matching Python's
///    `" ".join(pieces).strip()` at line 201).
pub fn extract_html(raw_html: &str) -> String {
    let mut html = Html::parse_document(raw_html);
    strip_unwanted_nodes(&mut html);

    let mut pieces: Vec<String> = Vec::new();

    for parsed_rule in PARSED_RULES.iter() {
        let rule = &parsed_rule.rule;
        let attr = rule.attr.as_deref();

        let text = if rule.all {
            // Select all matching elements.
            let texts: Vec<String> = html
                .select(&parsed_rule.selector)
                .map(|el| extract_text_from_element(&el, attr))
                .filter(|t| !t.is_empty())
                .collect();

            if texts.is_empty() {
                continue;
            }
            texts.join(&rule.join_with)
        } else {
            // Select the first matching element only.
            match html.select(&parsed_rule.selector).next() {
                Some(el) => extract_text_from_element(&el, attr),
                None => continue,
            }
        };

        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            pieces.push(trimmed);
        }
    }

    pieces.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rules_parse() {
        // Accessing PARSED_RULES forces the LazyLock to initialize.
        assert_eq!(PARSED_RULES.len(), 39, "expected 39 rules from TOML");
    }

    #[test]
    fn test_all_selectors_compile() {
        // If any selector failed to compile, the LazyLock init would have panicked.
        // This test simply accesses each rule to confirm no panic.
        for (i, parsed) in PARSED_RULES.iter().enumerate() {
            assert!(!parsed.rule.name.is_empty(), "rule {i} has an empty name");
        }
    }

    #[test]
    fn test_strip_script() {
        let html = r#"<html><body><p>Hello</p><script>alert('xss')</script></body></html>"#;
        let result = extract_html(html);
        assert!(
            !result.contains("alert"),
            "script content should be stripped"
        );
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_strip_style() {
        let html = r#"<html><body><p>Hello</p><style>body{color:red}</style></body></html>"#;
        let result = extract_html(html);
        assert!(
            !result.contains("color:red"),
            "style content should be stripped"
        );
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_strip_noscript() {
        let html =
            r#"<html><body><p>Hello</p><noscript>Enable JavaScript</noscript></body></html>"#;
        let result = extract_html(html);
        assert!(
            !result.contains("Enable JavaScript"),
            "noscript content should be stripped"
        );
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_strip_comments() {
        let html = r#"<html><body><!-- secret comment --><p>Visible</p></body></html>"#;
        let result = extract_html(html);
        assert!(
            !result.contains("secret comment"),
            "HTML comments should be stripped"
        );
        assert!(result.contains("Visible"));
    }

    #[test]
    fn test_meta_og_title() {
        let html = r#"<html><head><meta property="og:title" content="My OG Title"></head><body></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("My OG Title"),
            "og:title should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_meta_description() {
        let html = r#"<html><head><meta name="description" content="Page description here"></head><body></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("Page description here"),
            "meta description should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_img_alt() {
        let html = r#"<html><body><img alt="A cute cat" src="cat.jpg"></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("A cute cat"),
            "img alt text should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_role_based_div() {
        let html = r#"<html><body><div role="main">Main content here</div></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("Main content here"),
            "role-based div should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_table_text() {
        let html =
            r#"<html><body><table><tr><td>Cell A</td><td>Cell B</td></tr></table></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("Cell A"),
            "table cell text should be extracted; got: {result}"
        );
        assert!(
            result.contains("Cell B"),
            "table cell text should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_empty_html() {
        let html = r#"<html><head></head><body></body></html>"#;
        let result = extract_html(html);
        assert_eq!(result, "", "empty HTML should produce empty string");
    }

    #[test]
    fn test_headings_extraction() {
        let html =
            r#"<html><body><h1>Title One</h1><h2>Subtitle</h2><h3>Section</h3></body></html>"#;
        let result = extract_html(html);
        assert!(result.contains("Title One"), "h1 should be extracted");
        assert!(result.contains("Subtitle"), "h2 should be extracted");
        assert!(result.contains("Section"), "h3 should be extracted");
    }

    #[test]
    fn test_title_extraction() {
        let html = r#"<html><head><title>My Page Title</title></head><body></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("My Page Title"),
            "title should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_link_text_extraction() {
        let html = r#"<html><body><a href="https://example.com">Click here</a></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("Click here"),
            "link text should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_paragraph_extraction() {
        let html = r#"<html><body><p>First paragraph.</p><p>Second paragraph.</p></body></html>"#;
        let result = extract_html(html);
        assert!(result.contains("First paragraph."));
        assert!(result.contains("Second paragraph."));
    }

    #[test]
    fn test_blockquote_extraction() {
        let html = r#"<html><body><blockquote>A famous quote</blockquote></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("A famous quote"),
            "blockquote should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_code_block_extraction() {
        let html = r#"<html><body><code>let x = 42;</code></body></html>"#;
        let result = extract_html(html);
        assert!(
            result.contains("let x = 42;"),
            "code block should be extracted; got: {result}"
        );
    }

    #[test]
    fn test_emphasis_tags() {
        let html = r#"<html><body><strong>Bold text</strong> <em>Italic text</em> <mark>Highlighted</mark></body></html>"#;
        let result = extract_html(html);
        assert!(result.contains("Bold text"), "strong should be extracted");
        assert!(result.contains("Italic text"), "em should be extracted");
        assert!(result.contains("Highlighted"), "mark should be extracted");
    }
}
