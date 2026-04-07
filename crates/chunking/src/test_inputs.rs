//! Shared test input corpus for chunking tests.
//!
//! Ported from Python `cognee/tests/unit/processing/chunks/test_input.py`.
//! Each constant exercises a different edge case in the chunking pipeline.

// Constants are defined here and consumed by `#[cfg(test)]` modules in
// sibling files.  Allow dead-code so clippy doesn't complain before the
// test functions that reference them are added.
#![allow(dead_code)]

/// Empty input — zero-length string.
pub const EMPTY: &str = "";

/// Minimal non-empty input — single character without whitespace.
pub const SINGLE_CHAR: &str = "x";

/// Whitespace-only — spaces, tabs, CR+LF with no visible content.
pub const WHITESPACE: &str = "   \n\t   \r\n   ";

/// Multi-byte Unicode — emoji, Arabic, Hebrew combining chars.
pub const UNICODE_SPECIAL: &str = "Hello 👋 مرحبا שָׁלוֹם";

/// Mixed line endings — `\r\n`, `\n`, and `\r\n` in the same string.
pub const MIXED_ENDINGS: &str = "line1\r\nline2\nline3\r\nline4";

/// Leading and trailing blank lines around a single word.
pub const MANY_NEWLINES: &str = "\n\n\n\ntext\n\n\n\n";

/// HTML tags interleaved with plain text.
pub const HTML_MIXED: &str = "<p>Hello</p>\nPlain text\n<div>World</div>";

/// URLs and email addresses embedded in a sentence.
pub const URLS_EMAILS: &str = "Visit https://example.com or email user@example.com";

/// ASCII ellipsis `...` and Unicode ellipsis `…` (U+2026).
pub const ELLIPSES: &str = "Hello...How are you\u{2026}";

/// Structured English list with bullets, headers, and multi-paragraph layout.
pub const ENGLISH_LISTS: &str = include_str!("test_data/english_lists.txt");

/// Python source code with type hints, imports, overloads, and special chars.
pub const PYTHON_CODE: &str = include_str!("test_data/python_code.txt");

/// Long English literary text — excerpt from Paradise Lost (100+ lines).
pub const ENGLISH_TEXT: &str = include_str!("test_data/english_text.txt");

/// Continuous CJK characters with no space-delimited word boundaries.
/// Tests behaviour when `WordCounter` sees only 1 token per sentence-ending
/// segment because Chinese text contains no ASCII spaces.
pub const CHINESE_TEXT: &str = include_str!("test_data/chinese_text.txt");

/// All inputs from the standard corpus (analogous to Python's `INPUT_TEXTS`).
pub const ALL_INPUTS: &[(&str, &str)] = &[
    ("empty", EMPTY),
    ("single_char", SINGLE_CHAR),
    ("whitespace", WHITESPACE),
    ("unicode_special", UNICODE_SPECIAL),
    ("mixed_endings", MIXED_ENDINGS),
    ("many_newlines", MANY_NEWLINES),
    ("html_mixed", HTML_MIXED),
    ("urls_emails", URLS_EMAILS),
    ("ellipses", ELLIPSES),
    ("english_lists", ENGLISH_LISTS),
    ("python_code", PYTHON_CODE),
    ("english_text", ENGLISH_TEXT),
    ("chinese_text", CHINESE_TEXT),
];
