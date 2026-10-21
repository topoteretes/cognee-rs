//! Repair of a final `startxref` that points at whitespace just before `xref`.
//!
//! Some PDF writers emit an incremental update whose final `startxref` offset
//! is a few bytes short: it lands on the end-of-line directly in front of the
//! `xref` keyword instead of on the keyword itself. MEASURED on a corpus of
//! 164 vendor technical data sheets (SDK-1193): 161 of them carry a signature
//! added as an incremental update, and every one of those records the offset
//! of the `\n` before `xref`. Acrobat, browsers, pypdf and PDFium read
//! them; `lopdf` 0.38 (behind `pdf-extract` 0.10) does not, because it parses
//! the table with a strict `tag("xref")` at exactly the recorded offset and
//! reports `couldn't parse input: invalid file trailer`.
//!
//! [`repair_final_startxref`] moves the offset forward onto the keyword, in a
//! copy of the bytes handed to the parser only. The stored document is never
//! modified (it could not be anyway without invalidating the signature, whose
//! `/ByteRange` covers the trailer).
//!
//! The rewrite is deliberately narrow: it fires only when the bytes at the
//! recorded offset are 1 to [`MAX_LEADING_WHITESPACE`] PDF whitespace bytes
//! followed by `xref`. At such an offset `lopdf` fails unconditionally — neither
//! its xref-table nor its xref-stream branch skips leading whitespace — so the
//! repair can only turn a certain parse failure into a parse, never change how
//! an already readable file is read. Every other input, malformed or not, is
//! returned untouched and left for the parser to accept or reject.
//!
//! `lopdf` 0.45 tolerates this on its own (J-F-Liu/lopdf#555). Once a
//! `pdf-extract` release depends on it, this module can be deleted.

use std::borrow::Cow;
use std::ops::Range;

/// How far from the end of the file the final `%%EOF` is looked for. `lopdf`
/// searches the last 512 bytes; a marker outside that window is already fatal
/// to it, so a larger window would not make anything readable.
const EOF_SEARCH_WINDOW: usize = 512;

/// Upper bound on the whitespace skipped between the recorded offset and
/// `xref`. The corpus needs 1; a CRLF or a blank line needs at most 4.
const MAX_LEADING_WHITESPACE: usize = 4;

/// More digits than any `usize` offset can have; longer runs are rejected
/// before parsing.
const MAX_OFFSET_DIGITS: usize = 20;

/// A `startxref` value that points at whitespace in front of `xref`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StartxrefCorrection {
    /// Byte range of the offset's digits within the file.
    digits: Range<usize>,
    /// The offset the file records.
    pub(super) recorded: usize,
    /// The offset of the `xref` keyword it was meant to point at.
    pub(super) corrected: usize,
}

/// Returns `bytes` with the final `startxref` offset moved onto the `xref`
/// keyword when it points at up to [`MAX_LEADING_WHITESPACE`] whitespace bytes
/// in front of it, and the correction applied. Any other input is returned
/// borrowed and unchanged.
pub(super) fn repair_final_startxref(bytes: &[u8]) -> (Cow<'_, [u8]>, Option<StartxrefCorrection>) {
    let Some(correction) = find_correction(bytes) else {
        return (Cow::Borrowed(bytes), None);
    };
    let corrected = correction.corrected.to_string();
    let mut repaired = Vec::with_capacity(bytes.len() + 1);
    repaired.extend_from_slice(&bytes[..correction.digits.start]);
    repaired.extend_from_slice(corrected.as_bytes());
    repaired.extend_from_slice(&bytes[correction.digits.end..]);
    (Cow::Owned(repaired), Some(correction))
}

fn find_correction(bytes: &[u8]) -> Option<StartxrefCorrection> {
    let window_start = bytes.len().saturating_sub(EOF_SEARCH_WINDOW);
    let eof = window_start + rfind(&bytes[window_start..], b"%%EOF")?;
    let keyword = rfind(&bytes[..eof], b"startxref")?;

    let mut digits_start = keyword + b"startxref".len();
    while digits_start < eof && is_pdf_whitespace(bytes[digits_start]) {
        digits_start += 1;
    }
    let digit_count = bytes[digits_start..eof]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digit_count == 0 || digit_count > MAX_OFFSET_DIGITS {
        return None;
    }
    let digits = digits_start..digits_start + digit_count;
    // Only whitespace may separate the number from `%%EOF`; anything else
    // (a sign, a decimal point, garbage) is not a value worth second-guessing.
    if !bytes[digits.end..eof].iter().all(|&b| is_pdf_whitespace(b)) {
        return None;
    }
    let recorded: usize = std::str::from_utf8(&bytes[digits.clone()])
        .ok()?
        .parse()
        .ok()?;

    let at_offset = bytes.get(recorded..)?;
    let whitespace = at_offset
        .iter()
        .take(MAX_LEADING_WHITESPACE + 1)
        .take_while(|&&b| is_pdf_whitespace(b))
        .count();
    if whitespace == 0
        || whitespace > MAX_LEADING_WHITESPACE
        || !at_offset[whitespace..].starts_with(b"xref")
    {
        return None;
    }
    Some(StartxrefCorrection {
        digits,
        recorded,
        corrected: recorded + whitespace,
    })
}

/// The six whitespace characters of ISO 32000-1 §7.2.2.
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// A file tail whose final `startxref` records `offset`, with `xref`
    /// preceded by `gap` at byte 7.
    fn tail(gap: &str, offset: usize) -> Vec<u8> {
        format!("endobj\n{gap}xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n{offset}\n%%EOF\n")
            .into_bytes()
    }

    fn assert_untouched(bytes: &[u8]) {
        let (out, correction) = repair_final_startxref(bytes);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "input must be returned borrowed"
        );
        assert_eq!(correction, None);
        assert_eq!(&*out, bytes);
    }

    #[test]
    fn moves_an_offset_one_byte_short_onto_xref() {
        let bytes = tail("\n", 7);
        let (out, correction) = repair_final_startxref(&bytes);
        let correction = correction.expect("the offset points at the LF before xref");
        assert_eq!((correction.recorded, correction.corrected), (7, 8));
        assert_eq!(&*out, &tail("\n", 8)[..]);
    }

    #[test]
    fn skips_up_to_four_whitespace_bytes() {
        for gap in ["\r\n", " \r\n", "\r\n\r\n", "\0\t\x0C "] {
            let bytes = tail(gap, 7);
            let (out, correction) = repair_final_startxref(&bytes);
            assert_eq!(
                correction.map(|c| c.corrected),
                Some(7 + gap.len()),
                "gap {gap:?}"
            );
            assert_eq!(&*out, &tail(gap, 7 + gap.len())[..]);
        }
    }

    #[test]
    fn leaves_five_whitespace_bytes_alone() {
        assert_untouched(&tail("\n\n\n\n\n", 7));
    }

    #[test]
    fn grows_the_number_when_the_corrected_offset_needs_another_digit() {
        let mut bytes = vec![b'%'; 98];
        bytes.extend_from_slice(b"\n\nxref\n");
        bytes.extend_from_slice(b"startxref\n99\n%%EOF");
        let (out, correction) = repair_final_startxref(&bytes);
        assert_eq!(correction.map(|c| c.corrected), Some(100));
        assert!(out.ends_with(b"startxref\n100\n%%EOF"));
        assert_eq!(&out[100..104], b"xref");
    }

    #[test]
    fn leaves_a_correct_offset_alone() {
        assert_untouched(&tail("\n", 8));
    }

    #[test]
    fn leaves_an_offset_at_an_xref_stream_object_alone() {
        assert_untouched(b"%PDF-1.5\n12 0 obj\n<< /Type /XRef >>\nendobj\nstartxref\n9\n%%EOF\n");
    }

    #[test]
    fn leaves_whitespace_not_followed_by_xref_alone() {
        assert_untouched(&tail("\nfoo\n", 7));
    }

    #[test]
    fn only_the_final_startxref_is_considered() {
        // The first revision's offset is also short, but only the last one is
        // the one a reader starts from; `/Prev` chains are not rewritten.
        let mut bytes = tail("\n", 7);
        let second = bytes.len();
        bytes.extend_from_slice(&tail("\n", second + 8));
        let (out, correction) = repair_final_startxref(&bytes);
        assert_eq!(correction, None, "final offset is correct");
        assert_eq!(&*out, &bytes[..]);
    }

    #[test]
    fn malformed_tails_are_returned_untouched() {
        let cases: [&[u8]; 9] = [
            b"",
            b"%%EOF",
            b"endobj\n\nxref\n%%EOF\n",                    // no startxref
            b"endobj\n\nxref\nstartxref\n%%EOF\n",         // no number
            b"endobj\n\nxref\nstartxref\nseven\n%%EOF\n",  // non-numeric
            b"endobj\n\nxref\nstartxref\n-7\n%%EOF\n",     // signed
            b"endobj\n\nxref\nstartxref\n7.0\n%%EOF\n",    // fractional
            b"endobj\n\nxref\nstartxref\n999999\n%%EOF\n", // past the end
            b"endobj\n\nxref\nstartxref\n99999999999999999999999\n%%EOF\n", // overflow
        ];
        for bytes in cases {
            assert_untouched(bytes);
        }
        // No `%%EOF` at all.
        assert_untouched(b"endobj\n\nxref\nstartxref\n7\n");
        // `%%EOF` only outside the window `lopdf` searches.
        let mut far = tail("\n", 7);
        far.extend(std::iter::repeat_n(b' ', EOF_SEARCH_WINDOW));
        let (_, correction) = repair_final_startxref(&far);
        assert_eq!(correction, None);
    }

    #[test]
    fn an_offset_at_the_very_end_does_not_panic() {
        let mut bytes = b"startxref\n".to_vec();
        let len = bytes.len() + "NN\n%%EOF".len();
        bytes.extend_from_slice(format!("{len}\n%%EOF").as_bytes());
        assert_untouched(&bytes);
    }
}
