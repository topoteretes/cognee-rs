//! EML (email) text extraction via `mail-parser`.
//!
//! Parses RFC 5322 / MIME email messages, extracting headers
//! (From, To, Subject, Date) and the body text. Prefers plain-text
//! body; falls back to HTML body with tag stripping via `scraper`.
//! Headers and body are joined with `"\n\n"`.

use mail_parser::{Address, MessageParser};
use scraper::Html;

use super::super::LoaderError;

/// Extract text from an EML file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let message = MessageParser::default().parse(bytes).ok_or_else(|| {
        LoaderError::ExtractionFailed("Failed to parse email message".to_string())
    })?;

    let mut elements: Vec<String> = Vec::new();

    // Extract headers
    let mut headers: Vec<String> = Vec::new();

    if let Some(from) = message.from() {
        let from_str = format_address(from);
        if !from_str.is_empty() {
            headers.push(format!("From: {from_str}"));
        }
    }

    if let Some(to) = message.to() {
        let to_str = format_address(to);
        if !to_str.is_empty() {
            headers.push(format!("To: {to_str}"));
        }
    }

    if let Some(subject) = message.subject()
        && !subject.is_empty()
    {
        headers.push(format!("Subject: {subject}"));
    }

    if let Some(date) = message.date() {
        headers.push(format!("Date: {date}"));
    }

    if !headers.is_empty() {
        elements.push(headers.join("\n"));
    }

    // Extract body: prefer plain text, fall back to HTML with tag stripping
    let body_text = message
        .body_text(0)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let body = if let Some(text) = body_text {
        Some(text)
    } else {
        message
            .body_html(0)
            .map(|html| strip_html(&html))
            .filter(|t| !t.is_empty())
    };

    if let Some(body) = body {
        elements.push(body);
    }

    Ok(elements.join("\n\n"))
}

/// Format an address (From, To) as a readable string.
fn format_address(addr: &Address<'_>) -> String {
    match addr {
        Address::List(list) => list
            .iter()
            .map(|a| format_single_addr(a))
            .collect::<Vec<_>>()
            .join(", "),
        Address::Group(groups) => groups
            .iter()
            .map(|g| {
                let addrs: Vec<String> =
                    g.addresses.iter().map(|a| format_single_addr(a)).collect();
                match &g.name {
                    Some(name) => format!("{name}: {}", addrs.join(", ")),
                    None => addrs.join(", "),
                }
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

fn format_single_addr(addr: &mail_parser::Addr<'_>) -> String {
    match (&addr.name, &addr.address) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (None, Some(email)) => email.to_string(),
        (Some(name), None) => name.to_string(),
        (None, None) => String::new(),
    }
}

/// Strip HTML tags from content, returning plain text.
fn strip_html(html: &str) -> String {
    let document = Html::parse_document(html);
    document
        .root_element()
        .text()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
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
    fn parse_simple_email() {
        let eml = b"From: sender@example.com\r\n\
                     To: recipient@example.com\r\n\
                     Subject: Test Email\r\n\
                     Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\
                     \r\n\
                     Hello, this is a test email body.\r\n";

        let result = extract(eml).unwrap();
        assert!(result.contains("From:"));
        assert!(result.contains("sender@example.com"));
        assert!(result.contains("To:"));
        assert!(result.contains("recipient@example.com"));
        assert!(result.contains("Subject: Test Email"));
        assert!(result.contains("Hello, this is a test email body."));
    }

    #[test]
    fn parse_email_without_body() {
        let eml = b"From: sender@example.com\r\n\
                     Subject: No Body\r\n\
                     \r\n";

        let result = extract(eml).unwrap();
        assert!(result.contains("Subject: No Body"));
    }

    #[test]
    fn invalid_email_returns_error() {
        let result = extract(b"");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, LoaderError::ExtractionFailed(_)),
            "expected ExtractionFailed, got {err:?}"
        );
    }

    #[test]
    fn strip_html_basic() {
        let result = strip_html("<html><body><p>Hello</p></body></html>");
        assert!(result.contains("Hello"));
        assert!(!result.contains("<p>"));
    }
}
