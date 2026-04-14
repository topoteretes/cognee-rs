use cognee_ingestion::url_crawler::{HtmlParser, extract_html};

#[test]
fn test_html_extraction_snapshot() {
    let html = include_str!("fixtures/html/sample.html");
    let expected = include_str!("fixtures/html/sample.expected.txt");
    let actual = extract_html(html);

    // Allow regeneration of the expected output via UPDATE_SNAPSHOTS=1.
    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/html/sample.expected.txt"
            ),
            &actual,
        )
        .expect("failed to update snapshot file");
    }

    assert_eq!(
        actual.trim(),
        expected.trim(),
        "snapshot mismatch: run with UPDATE_SNAPSHOTS=1 to regenerate"
    );
}

#[test]
fn test_parser_extract_text_delegates() {
    let html = r#"<html><head><title>Delegation Test</title></head>
        <body><p>Some content.</p></body></html>"#;
    let via_parser = HtmlParser::extract_text(html);
    let via_rules = extract_html(html);
    assert_eq!(
        via_parser, via_rules,
        "HtmlParser::extract_text should delegate to extract_html"
    );
}

#[test]
fn test_parser_extract_title_unchanged() {
    let html = r#"<html><head><title>My Title</title></head><body></body></html>"#;
    let title = HtmlParser::extract_title(html);
    assert_eq!(title, Some("My Title".to_string()));
}

#[test]
fn test_script_style_noscript_stripped_in_full_document() {
    let html = include_str!("fixtures/html/sample.html");
    let result = extract_html(html);
    assert!(
        !result.contains("should not appear"),
        "script content 'should not appear' was found in output"
    );
    assert!(
        !result.contains("color: red"),
        "style content was found in output"
    );
    assert!(
        !result.contains("noscript text should not appear"),
        "noscript content was found in output"
    );
    assert!(
        !result.contains("This comment should not appear"),
        "HTML comment content was found in output"
    );
    assert!(
        !result.contains("invisible"),
        "second script block content was found in output"
    );
}
