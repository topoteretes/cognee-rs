//! Text helpers shared by ingestion and chunking.

/// Translate line endings as Python's text mode does (universal newlines):
/// `\r\n` and a lone `\r` both become `\n`.
///
/// Python reads loader input and stored text with `open(path,
/// encoding=...)`, which translates them, so its chunks never hold `\r`.
pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}
