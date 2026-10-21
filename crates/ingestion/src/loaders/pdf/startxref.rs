//! Repair of xref offsets that point at whitespace just before `xref`.
//!
//! Some PDF writers emit an incremental update whose `startxref` offset is a
//! few bytes short: it lands on the end-of-line directly in front of the
//! `xref` keyword instead of on the keyword itself. MEASURED on a corpus of
//! 164 vendor technical data sheets (SDK-1193): 161 of them carry a signature
//! added as an incremental update, and every one of those records the offset
//! of the `\n` before `xref`. Acrobat, browsers, pypdf and PDFium read them;
//! `lopdf` 0.38 (behind `pdf-extract` 0.10) does not, because it parses the
//! table with a strict `tag("xref")` at exactly the recorded offset and
//! reports `couldn't parse input: invalid file trailer`.
//!
//! The same writer updating a file a second time (a second signature) copies
//! its short offset into the new trailer's `/Prev`, and `lopdf` follows
//! `/Prev` just as strictly, so [`repair_xref_offsets`] walks that chain too.
//!
//! The caller only reaches for this after `pdf-extract` has already rejected
//! the bytes, so a file that parses is never rewritten. The repair works on a
//! copy; the stored document is untouched (it could not be modified anyway
//! without invalidating the signature, whose `/ByteRange` covers the trailer).
//!
//! What is rewritten, and only that:
//!
//! - the final `startxref`, found the way `lopdf` finds it (the last `%%EOF`
//!   in the final 512 bytes, `startxref` at most 25 bytes before it);
//! - each `/Prev` in the chain of classic xref-table trailers it leads to;
//!
//! and each only when the bytes at the recorded offset are 1 to
//! [`MAX_LEADING_WHITESPACE`] PDF whitespace bytes followed by `xref`. A
//! `/Prev` keeps its width (it is zero-padded, which `lopdf` accepts): a longer
//! number would shift every later byte offset in the file, so a `/Prev` that
//! needs another digit is left alone. The final `startxref` may grow, since
//! nothing after it is addressed by offset, as long as it stays within the
//! 25-byte window. Xref streams (`N G obj` at the offset) end the walk.
//!
//! `lopdf` 0.45 tolerates this on its own (J-F-Liu/lopdf#555). Once a
//! `pdf-extract` release depends on it, this module can be deleted.

use std::ops::Range;

/// How far from the end of the file the final `%%EOF` is looked for. `lopdf`
/// searches the last 512 bytes; a marker outside that window is already fatal
/// to it, so a larger window would not make anything readable.
const EOF_SEARCH_WINDOW: usize = 512;

/// How far before `%%EOF` the `startxref` keyword may start. `lopdf` searches
/// from `%%EOF - 25`, so a keyword earlier than that is never found by it.
const STARTXREF_SEARCH_WINDOW: usize = 25;

/// Upper bound on the whitespace skipped between a recorded offset and
/// `xref`. The corpus needs 1; a CRLF or a blank line needs at most 4.
const MAX_LEADING_WHITESPACE: usize = 4;

/// More digits than any `usize` offset can have; longer runs are rejected
/// before parsing.
const MAX_OFFSET_DIGITS: usize = 20;

/// Bound on the `/Prev` chain walked, against cycles the visited set misses
/// and pathological inputs.
const MAX_PREV_CHAIN: usize = 64;

/// Which offset a correction applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OffsetField {
    /// The final `startxref` value.
    Startxref,
    /// A trailer's `/Prev` value.
    Prev,
}

/// One rewritten offset.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OffsetCorrection {
    /// Byte range of the offset's digits within the original file.
    digits: Range<usize>,
    pub(super) field: OffsetField,
    /// The offset the file records.
    pub(super) recorded: usize,
    /// The offset of the `xref` keyword it was meant to point at.
    pub(super) corrected: usize,
}

/// A repaired copy of a PDF and the offsets that were changed in it.
#[derive(Debug)]
pub(super) struct XrefRepair {
    pub(super) bytes: Vec<u8>,
    /// Ordered by position in the file, so the final `startxref` comes last.
    pub(super) corrections: Vec<OffsetCorrection>,
}

/// Returns a copy of `bytes` in which the final `startxref` and the `/Prev`
/// chain behind it point at their `xref` keywords, or `None` when no offset in
/// that chain is a repairable near miss.
pub(super) fn repair_xref_offsets(bytes: &[u8]) -> Option<XrefRepair> {
    let tail = final_startxref(bytes)?;
    let mut corrections = Vec::new();

    let mut xref = resolve_xref_table(bytes, tail.number.value)?;
    if xref != tail.number.value {
        let growth = digit_count(xref).saturating_sub(tail.number.digits.len());
        if tail.eof + growth - tail.keyword > STARTXREF_SEARCH_WINDOW {
            // The longer number would push `startxref` out of the window
            // `lopdf` searches, and the copy would fail differently.
            return None;
        }
        corrections.push(OffsetCorrection {
            digits: tail.number.digits.clone(),
            field: OffsetField::Startxref,
            recorded: tail.number.value,
            corrected: xref,
        });
    }

    let mut visited = vec![xref];
    for _ in 0..MAX_PREV_CHAIN {
        let Some(prev) = trailer_prev(bytes, xref) else {
            break;
        };
        let Some(prev_xref) = resolve_xref_table(bytes, prev.value) else {
            break;
        };
        // A cycle can lead back to a `/Prev` already corrected; rewriting the
        // same digits twice would overlap in the copy below.
        let seen = corrections.iter().any(|c| c.digits == prev.digits);
        if prev_xref != prev.value && !seen {
            if digit_count(prev_xref) > prev.digits.len() {
                break;
            }
            corrections.push(OffsetCorrection {
                digits: prev.digits,
                field: OffsetField::Prev,
                recorded: prev.value,
                corrected: prev_xref,
            });
        }
        if visited.contains(&prev_xref) {
            break;
        }
        visited.push(prev_xref);
        xref = prev_xref;
    }

    if corrections.is_empty() {
        return None;
    }
    corrections.sort_by_key(|c| c.digits.start);

    let mut repaired = Vec::with_capacity(bytes.len() + 1);
    let mut copied = 0;
    for c in &corrections {
        repaired.extend_from_slice(&bytes[copied..c.digits.start]);
        let width = c.digits.len();
        repaired.extend_from_slice(format!("{:0width$}", c.corrected).as_bytes());
        copied = c.digits.end;
    }
    repaired.extend_from_slice(&bytes[copied..]);
    Some(XrefRepair {
        bytes: repaired,
        corrections,
    })
}

/// A decimal offset as written in the file.
struct Number {
    digits: Range<usize>,
    value: usize,
}

/// The final `startxref` and where it sits relative to `%%EOF`.
struct Tail {
    eof: usize,
    keyword: usize,
    number: Number,
}

fn final_startxref(bytes: &[u8]) -> Option<Tail> {
    let window_start = bytes.len().saturating_sub(EOF_SEARCH_WINDOW);
    let eof = window_start + rfind(&bytes[window_start..], b"%%EOF")?;
    let search_start = eof.checked_sub(STARTXREF_SEARCH_WINDOW)?;
    let keyword = search_start + rfind(&bytes[search_start..eof], b"startxref")?;

    let number = number_after(bytes, keyword + b"startxref".len(), eof)?;
    // Only whitespace may separate the number from `%%EOF`; anything else
    // (a sign, a decimal point, garbage) is not a value worth second-guessing.
    if !bytes[number.digits.end..eof]
        .iter()
        .all(|&b| is_pdf_whitespace(b))
    {
        return None;
    }
    Some(Tail {
        eof,
        keyword,
        number,
    })
}

/// The `/Prev` of the trailer that follows the classic xref table at `xref`.
///
/// A classic table holds only digits, `f`, `n` and whitespace, so the first
/// non-table byte after the keyword must start `trailer`. The dictionary is
/// taken to run to the next `xref`: the tail of the `startxref` that should
/// follow it, or, when that is missing, the next section's keyword, so a
/// malformed trailer never reads another section's `/Prev`.
fn trailer_prev(bytes: &[u8], xref: usize) -> Option<Number> {
    let body = xref + b"xref".len();
    let table_len = bytes[body..]
        .iter()
        .take_while(|&&b| b.is_ascii_digit() || b == b'f' || b == b'n' || is_pdf_whitespace(b))
        .count();
    let trailer = body + table_len;
    if !bytes[trailer..].starts_with(b"trailer") {
        return None;
    }
    let dict_end = trailer + find(&bytes[trailer..], b"xref")?;
    let key = trailer + find(&bytes[trailer..dict_end], b"/Prev")?;
    let after_key = key + b"/Prev".len();
    // `/PrevFoo` is another name; the key must be followed by whitespace.
    if !bytes.get(after_key).is_some_and(|&b| is_pdf_whitespace(b)) {
        return None;
    }
    number_after(bytes, after_key, dict_end)
}

/// Skips whitespace from `start`, then reads a run of 1 to
/// [`MAX_OFFSET_DIGITS`] digits that ends before `limit`.
fn number_after(bytes: &[u8], start: usize, limit: usize) -> Option<Number> {
    let region = &bytes[start..limit];
    let skip = region.iter().take_while(|&&b| is_pdf_whitespace(b)).count();
    let count = region[skip..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if count == 0 || count > MAX_OFFSET_DIGITS {
        return None;
    }
    let digits = start + skip..start + skip + count;
    let value = std::str::from_utf8(&bytes[digits.clone()])
        .ok()?
        .parse()
        .ok()?;
    Some(Number { digits, value })
}

/// Where the classic xref table an `offset` refers to starts: `offset` itself
/// when `xref` is there, `offset` plus the gap when 1 to
/// [`MAX_LEADING_WHITESPACE`] whitespace bytes precede it, `None` otherwise
/// (an xref stream, garbage, or past the end).
fn resolve_xref_table(bytes: &[u8], offset: usize) -> Option<usize> {
    let at = bytes.get(offset..)?;
    let gap = at
        .iter()
        .take(MAX_LEADING_WHITESPACE + 1)
        .take_while(|&&b| is_pdf_whitespace(b))
        .count();
    (gap <= MAX_LEADING_WHITESPACE && at[gap..].starts_with(b"xref")).then_some(offset + gap)
}

fn digit_count(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

/// The six whitespace characters of ISO 32000-1 §7.2.2.
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// One classic xref section, `gap` whitespace in front of `xref`.
    fn section(gap: &str, prev: Option<String>) -> String {
        let prev = prev.map(|p| format!(" /Prev {p}")).unwrap_or_default();
        format!("endobj\n{gap}xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1{prev} >>\n")
    }

    /// A file of one section per entry of `shorts`, each section an
    /// incremental update of the one before. Section `i`'s offset is recorded
    /// `shorts[i]` bytes short, both in its own `startxref` and in the next
    /// section's `/Prev`, the way a writer that reuses its recorded value
    /// does. Returns the file and the true `xref` positions.
    fn chain(shorts: &[usize]) -> (Vec<u8>, Vec<usize>) {
        let mut pdf = String::from("%PDF-1.7\n");
        let mut xrefs = Vec::new();
        let mut prev_recorded = None;
        for &short in shorts {
            let gap = "\n".repeat(short.max(1));
            let start = pdf.len();
            pdf.push_str(&section(&gap, prev_recorded.map(|p: usize| p.to_string())));
            let xref = start + "endobj\n".len() + gap.len();
            let recorded = xref - short;
            pdf.push_str(&format!("startxref\n{recorded}\n%%EOF\n"));
            xrefs.push(xref);
            prev_recorded = Some(recorded);
        }
        (pdf.into_bytes(), xrefs)
    }

    fn fields(repair: &XrefRepair) -> Vec<(OffsetField, usize, usize)> {
        repair
            .corrections
            .iter()
            .map(|c| (c.field, c.recorded, c.corrected))
            .collect()
    }

    #[test]
    fn moves_a_final_startxref_one_byte_short_onto_xref() {
        let (bytes, xrefs) = chain(&[0, 1]);
        let repair = repair_xref_offsets(&bytes).expect("final offset is one short");
        assert_eq!(
            fields(&repair),
            [(OffsetField::Startxref, xrefs[1] - 1, xrefs[1])]
        );
        assert_eq!(repair.bytes, chain(&[0, 0]).0);
    }

    #[test]
    fn repairs_a_short_prev_left_by_an_earlier_update() {
        // Signed twice by the same tool: both its offsets are short.
        let (bytes, xrefs) = chain(&[0, 1, 1]);
        let repair = repair_xref_offsets(&bytes).expect("two short offsets");
        assert_eq!(
            fields(&repair),
            [
                (OffsetField::Prev, xrefs[1] - 1, xrefs[1]),
                (OffsetField::Startxref, xrefs[2] - 1, xrefs[2]),
            ]
        );
        // Exactly those two numbers change. The middle section's own
        // `startxref` stays short: it is not the final one, and no reader
        // starts from it.
        let expected = String::from_utf8(bytes)
            .unwrap()
            .replace(
                &format!("/Prev {} ", xrefs[1] - 1),
                &format!("/Prev {} ", xrefs[1]),
            )
            .replace(
                &format!("startxref\n{}\n%%EOF\n", xrefs[2] - 1),
                &format!("startxref\n{}\n%%EOF\n", xrefs[2]),
            );
        assert_eq!(repair.bytes, expected.into_bytes());
    }

    #[test]
    fn repairs_a_short_prev_behind_a_correct_final_offset() {
        let (bytes, xrefs) = chain(&[1, 0]);
        let repair = repair_xref_offsets(&bytes).expect("the /Prev is one short");
        assert_eq!(
            fields(&repair),
            [(OffsetField::Prev, xrefs[0] - 1, xrefs[0])]
        );
        // Only the `/Prev` changes; the first section's own `startxref` is not
        // the final one, and no reader starts from it.
        let mut expected = bytes.clone();
        let prev_at = find(&expected, b"/Prev ").unwrap() + b"/Prev ".len();
        let old = (xrefs[0] - 1).to_string();
        expected.splice(prev_at..prev_at + old.len(), xrefs[0].to_string().bytes());
        assert_eq!(repair.bytes, expected);
    }

    #[test]
    fn skips_up_to_four_whitespace_bytes() {
        for gap in ["\r\n", " \r\n", "\r\n\r\n", "\0\t\x0C "] {
            let mut bytes = section(gap, None).into_bytes();
            bytes.extend_from_slice(b"startxref\n7\n%%EOF\n");
            let repair = repair_xref_offsets(&bytes).expect(gap);
            assert_eq!(
                repair.corrections[0].corrected,
                7 + gap.len(),
                "gap {gap:?}"
            );
        }
    }

    #[test]
    fn leaves_five_whitespace_bytes_alone() {
        let mut bytes = section("\n\n\n\n\n", None).into_bytes();
        bytes.extend_from_slice(b"startxref\n7\n%%EOF\n");
        assert!(repair_xref_offsets(&bytes).is_none());
    }

    #[test]
    fn a_well_formed_chain_needs_no_repair() {
        assert!(repair_xref_offsets(&chain(&[0]).0).is_none());
        assert!(repair_xref_offsets(&chain(&[0, 0, 0]).0).is_none());
    }

    #[test]
    fn grows_the_final_number_when_it_needs_another_digit() {
        let mut bytes = vec![b'%'; 98];
        bytes.extend_from_slice(b"\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< >>\n");
        bytes.extend_from_slice(b"startxref\n99\n%%EOF");
        let repair = repair_xref_offsets(&bytes).expect("99 points at the LF before xref");
        assert!(repair.bytes.ends_with(b"startxref\n100\n%%EOF"));
        assert_eq!(&repair.bytes[100..104], b"xref");
    }

    #[test]
    fn declines_growth_that_would_push_startxref_out_of_the_window() {
        // `startxref` exactly 25 bytes before `%%EOF`: one more digit puts it
        // at 26, where `lopdf` no longer looks.
        let mut bytes = vec![b'%'; 98];
        bytes.extend_from_slice(b"\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< >>\n");
        bytes.extend_from_slice(b"startxref\n            99\n%%EOF");
        let eof = rfind(&bytes, b"%%EOF").unwrap();
        assert_eq!(
            eof - rfind(&bytes, b"startxref").unwrap(),
            STARTXREF_SEARCH_WINDOW
        );
        assert!(repair_xref_offsets(&bytes).is_none());
    }

    #[test]
    fn ignores_a_startxref_further_from_eof_than_lopdf_searches() {
        let mut bytes = section("\n", None).into_bytes();
        bytes.extend_from_slice(b"startxref\n7\n");
        bytes.extend(std::iter::repeat_n(b' ', STARTXREF_SEARCH_WINDOW));
        bytes.extend_from_slice(b"%%EOF\n");
        assert!(repair_xref_offsets(&bytes).is_none());
    }

    #[test]
    fn keeps_a_prev_at_its_width() {
        // A zero-padded `/Prev` is rewritten zero-padded, at the same width.
        let mut bytes = vec![b'%'; 98];
        bytes.extend_from_slice(b"\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< >>\n");
        let second = bytes.len() + b"endobj\n".len();
        bytes.extend_from_slice(
            format!("endobj\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Prev 00099 >>\nstartxref\n{second}\n%%EOF")
                .as_bytes(),
        );
        let repair = repair_xref_offsets(&bytes).expect("/Prev 99 is one short");
        assert_eq!(fields(&repair), [(OffsetField::Prev, 99, 100)]);
        assert_eq!(repair.bytes.len(), bytes.len());
        assert!(find(&repair.bytes, b"/Prev 00100 ").is_some());
    }

    #[test]
    fn declines_a_prev_that_would_need_another_digit() {
        let mut bytes = vec![b'%'; 98];
        bytes.extend_from_slice(b"\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< >>\n");
        let second = bytes.len() + b"endobj\n".len();
        bytes.extend_from_slice(
            format!("endobj\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Prev 99 >>\nstartxref\n{second}\n%%EOF")
                .as_bytes(),
        );
        assert!(repair_xref_offsets(&bytes).is_none());
    }

    #[test]
    fn stops_at_an_xref_stream() {
        let mut bytes = b"%PDF-1.5\n12 0 obj\n<< /Type /XRef >>\nendobj\n".to_vec();
        let second = bytes.len() + b"endobj\n\n".len();
        bytes.extend_from_slice(
            format!("endobj\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Prev 9 >>\nstartxref\n{}\n%%EOF", second - 1)
                .as_bytes(),
        );
        let repair = repair_xref_offsets(&bytes).expect("final offset is one short");
        assert_eq!(
            fields(&repair),
            [(OffsetField::Startxref, second - 1, second)]
        );
    }

    #[test]
    fn a_prev_cycle_terminates() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let xref = bytes.len() + b"endobj\n\n".len();
        bytes.extend_from_slice(
            format!("endobj\n\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Prev {} >>\nstartxref\n{}\n%%EOF", xref - 1, xref - 1)
                .as_bytes(),
        );
        let repair = repair_xref_offsets(&bytes).expect("both offsets are one short");
        assert_eq!(repair.corrections.len(), 2);
    }

    #[test]
    fn ignores_a_name_that_only_starts_with_prev() {
        let (bytes, _) = chain(&[1, 0]);
        let renamed = String::from_utf8(bytes)
            .unwrap()
            .replace("/Prev ", "/PrevX ");
        assert!(repair_xref_offsets(renamed.as_bytes()).is_none());
    }

    #[test]
    fn malformed_tails_are_declined() {
        let cases: [&[u8]; 10] = [
            b"",
            b"%%EOF",
            b"endobj\n\nxref\n%%EOF\n",                    // no startxref
            b"endobj\n\nxref\nstartxref\n%%EOF\n",         // no number
            b"endobj\n\nxref\nstartxref\nseven\n%%EOF\n",  // non-numeric
            b"endobj\n\nxref\nstartxref\n-7\n%%EOF\n",     // signed
            b"endobj\n\nxref\nstartxref\n7.0\n%%EOF\n",    // fractional
            b"endobj\n\nxref\nstartxref\n999999\n%%EOF\n", // past the end
            b"endobj\n\nxref\nstartxref\n99999999999999999999999\n%%EOF\n", // overflow
            b"endobj\n\nxref\nstartxref\n7\n",             // no %%EOF
        ];
        for bytes in cases {
            assert!(
                repair_xref_offsets(bytes).is_none(),
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        // `%%EOF` only outside the window `lopdf` searches.
        let (mut far, _) = chain(&[1]);
        far.extend(std::iter::repeat_n(b' ', EOF_SEARCH_WINDOW));
        assert!(repair_xref_offsets(&far).is_none());
    }

    #[test]
    fn an_offset_at_the_very_end_does_not_panic() {
        // Padded so `%%EOF` is past byte 25 and the offset check is reached.
        let mut bytes = vec![b' '; 30];
        bytes.extend_from_slice(b"startxref\n");
        let len = bytes.len() + "NN\n%%EOF".len();
        bytes.extend_from_slice(format!("{len}\n%%EOF").as_bytes());
        assert_eq!(bytes.len(), len);
        assert!(repair_xref_offsets(&bytes).is_none());
    }

    #[test]
    fn a_table_running_to_the_end_of_the_file_does_not_panic() {
        let (mut bytes, xrefs) = chain(&[1, 0]);
        bytes.truncate(xrefs[0] + 10);
        bytes.extend_from_slice(b"\nstartxref\n");
        bytes.extend_from_slice(xrefs[0].to_string().as_bytes());
        bytes.extend_from_slice(b"\n%%EOF");
        assert!(repair_xref_offsets(&bytes).is_none());
    }
}
