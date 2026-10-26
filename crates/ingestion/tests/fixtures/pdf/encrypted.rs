//! Encrypted copies of `sample.pdf`, shared by the PDF fixture tests.
//!
//! Integration tests are separate crates, so this is pulled in with
//! `#[path = "fixtures/pdf/encrypted.rs"] mod encrypted;`. It sits in a
//! subdirectory of `tests/`, so Cargo does not build it as a test of its own.

/// `sample.pdf` encrypted with an EMPTY user password (and a non-empty owner
/// password), which any reader opens without prompting. See
/// `README.md` in this directory for how these were made. Read through
/// `pdf-extract` in `pdf_pure_rust_fixture_extraction.rs` and through PDFium in
/// `pdf_fixture_extraction.rs`.
///
/// `pdf-extract` 0.10 could not read any of them. Its `lopdf` 0.38 decrypts
/// every object at load time with the empty password but leaves `/Encrypt` in
/// the trailer; `pdf-extract` then sees `is_encrypted()` and decrypts a second
/// time. What that second pass did depended on the cipher:
///
/// - RC4 is symmetric, so it re-encrypted the streams. Inflating them failed,
///   every page came back empty, and the loader reported *success*: the
///   document was stored empty and nothing in it was searchable.
/// - AES rejected the already-decrypted streams
///   (`Decryption(InvalidCipherTextLength)`), so extraction failed outright.
///
/// `lopdf` 0.42 (behind `pdf-extract` 0.12) drops `/Encrypt` once it has
/// decrypted, so the second pass no longer happens.
pub const ENCRYPTED_EMPTY_USER_PASSWORD: [(&str, &[u8]); 3] = [
    (
        "sample.encrypted-rc4-128.pdf",
        include_bytes!("sample.encrypted-rc4-128.pdf"),
    ),
    (
        "sample.encrypted-aes-128.pdf",
        include_bytes!("sample.encrypted-aes-128.pdf"),
    ),
    (
        "sample.encrypted-aes-256.pdf",
        include_bytes!("sample.encrypted-aes-256.pdf"),
    ),
];
