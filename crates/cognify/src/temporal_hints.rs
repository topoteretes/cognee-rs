//! Deterministic date hints for graph extraction.
//!
//! Port of Python's `cognee/modules/engine/utils/temporal_hints.py`
//! (`hint_lines`, `document_temporal_hints`). Date expressions whose year is not
//! stated in the text ("of 27 April", "four weeks later") are resolved against a
//! rolling per-document base — the last date with a stated year seen earlier in
//! the document — and handed to the default graph prompt as normalization hints,
//! so the LLM names Timestamp nodes with absolute dates instead of guessing. The
//! chunk text itself is never modified.
//!
//! # Divergence from Python
//!
//! Python finds date expressions with `dateparser.search.search_dates` and
//! re-parses the year-less ones with `RELATIVE_BASE`. There is no Rust
//! equivalent of that library, so this finder recognises a deliberately small
//! set of shapes:
//!
//! * dates with a stated four-digit year (anything
//!   [`cognee_models::normalize_absolute_date`] accepts, including ISO forms) —
//!   these advance the base and produce no hint;
//! * a capitalised month name with an adjacent day and no year ("27 April",
//!   "April 27th"), or a bare one after a word that introduces it ("in May",
//!   "by June") — hinted at the base's year. A bare capitalised month alone is
//!   too often a sentence-initial "May …" or a first name to hint;
//! * `<number> <day|week|month|year>(s) <later|earlier|ago>` — hinted as an
//!   offset from the base.
//!
//! Python additionally resolves bare times of day and a few more relative
//! phrasings; those produce no hint here. A missed hint costs nothing (the raw
//! text is still in the prompt), while a bogus one is misinformation, so the
//! finder errs the same way Python's anchor gate does: towards precision.

use chrono::{Datelike, Duration, Months, NaiveDate};
use cognee_models::{TimestampPrecision, month_from_word, parse_time_period};

/// One whitespace-delimited word with surrounding punctuation stripped, and its
/// byte span in the source text.
struct Word<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut out = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive(char::is_whitespace) {
        let trimmed_start = raw.trim_start_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '"' | '\''
                )
        });
        let lead = raw.len() - trimmed_start.len();
        let trimmed = trimmed_start.trim_end_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '"' | '\''
                )
        });
        if !trimmed.is_empty() {
            out.push(Word {
                text: trimmed,
                start: offset + lead,
                end: offset + lead + trimmed.len(),
            });
        }
        offset += raw.len();
    }
    out
}

fn is_four_digit_year(word: &str) -> bool {
    word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit())
}

fn is_filler(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "of" | "on" | "the" | "in" | "at"
    )
}

/// A month named with a capital letter. Lower-case "may" is usually the verb,
/// and Python's anchor gate matches month names case-sensitively for the same
/// reason.
fn capitalised_month(word: &str) -> Option<u32> {
    word.chars()
        .next()
        .filter(char::is_ascii_uppercase)
        .and_then(|_| month_from_word(word))
}

fn day_number(word: &str) -> Option<u32> {
    let lower = word.to_ascii_lowercase();
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix))
        .unwrap_or(&lower);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|day| (1..=31).contains(day))
}

fn small_number(word: &str) -> Option<u32> {
    const WORDS: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    let lower = word.to_ascii_lowercase();
    if lower == "a" || lower == "an" {
        return Some(1);
    }
    if let Some(index) = WORDS.iter().position(|w| *w == lower) {
        return u32::try_from(index + 1).ok();
    }
    (lower.len() <= 3 && lower.bytes().all(|b| b.is_ascii_digit()))
        .then(|| lower.parse().ok())
        .flatten()
}

/// The longest run of up to five words starting at `index` that reads as an
/// absolute date with a stated year, with that date's lower bound.
fn absolute_at(text: &str, words: &[Word<'_>], index: usize) -> Option<(usize, NaiveDate)> {
    // "$1986" is money.
    if text[..words[index].start].ends_with('$') {
        return None;
    }
    for len in (1..=5.min(words.len() - index)).rev() {
        let run = &words[index..index + len];
        if !run
            .iter()
            .any(|w| is_four_digit_year(w.text) || w.text.contains('-'))
        {
            continue;
        }
        let candidate = &text[run[0].start..run[len - 1].end];
        if let Some(period) = parse_time_period(candidate)
            && period.precision != TimestampPrecision::Span
        {
            return Some((len, period.lower.date()));
        }
    }
    None
}

/// Words that make a bare month name a date: "in May", "by April", "since
/// June". Without a day, a capitalised month name is otherwise often a
/// sentence-initial "May …" or a first name ("April joined"), and a bogus hint
/// is misinformation.
fn introduces_a_month(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "in" | "of"
            | "by"
            | "since"
            | "until"
            | "till"
            | "during"
            | "before"
            | "after"
            | "from"
            | "through"
            | "early"
            | "late"
            | "mid"
    )
}

/// A capitalised month with an adjacent day, or a bare one after a word that
/// introduces it, and no year: `(words consumed, month, day)`.
fn yearless_at(words: &[Word<'_>], index: usize) -> Option<(usize, u32, Option<u32>)> {
    let first = words[index].text;
    let next = words.get(index + 1).map(|w| w.text);
    let (len, month, day) = if let Some(month) = capitalised_month(first) {
        match next.and_then(day_number) {
            Some(day) => (2, month, Some(day)),
            None if index > 0 && introduces_a_month(words[index - 1].text) => (1, month, None),
            None => return None,
        }
    } else {
        let day = day_number(first)?;
        let month = capitalised_month(next?)?;
        (2, month, Some(day))
    };
    // A year right after belongs to the expression; `absolute_at` would have
    // taken it, so this is a number that only looks like one (or a range).
    if words
        .get(index + len)
        .is_some_and(|w| is_four_digit_year(w.text))
    {
        return None;
    }
    Some((len, month, day))
}

/// `<n> <unit>(s) <later|earlier|ago>`: `(words consumed, signed offset)`.
fn relative_at(words: &[Word<'_>], index: usize) -> Option<(usize, i64, char)> {
    let n = i64::from(small_number(words[index].text)?);
    let unit = words.get(index + 1)?.text.to_ascii_lowercase();
    let unit = unit.strip_suffix('s').unwrap_or(&unit);
    let unit = match unit {
        "day" => 'd',
        "week" => 'w',
        "month" => 'm',
        "year" => 'y',
        _ => return None,
    };
    let sign = match words.get(index + 2)?.text.to_ascii_lowercase().as_str() {
        "later" => 1,
        "earlier" | "ago" => -1,
        _ => return None,
    };
    Some((3, sign * n, unit))
}

fn offset(base: NaiveDate, amount: i64, unit: char) -> Option<NaiveDate> {
    match unit {
        'd' => base.checked_add_signed(Duration::days(amount)),
        'w' => base.checked_add_signed(Duration::weeks(amount)),
        'm' | 'y' => {
            let months =
                u32::try_from(amount.unsigned_abs()).ok()? * if unit == 'y' { 12 } else { 1 };
            if amount >= 0 {
                base.checked_add_months(Months::new(months))
            } else {
                base.checked_sub_months(Months::new(months))
            }
        }
        _ => None,
    }
}

/// `(hint lines, updated base)` for one chunk of text.
///
/// Expressions with a stated year advance the base and need no hint; the
/// year-less ones are resolved against the base current at their position. A
/// hint names the source expression and marks the resolved date as inferred,
/// so the prompt can tell which parts came from context rather than the text.
pub fn hint_lines(text: &str, mut base: Option<NaiveDate>) -> (Vec<String>, Option<NaiveDate>) {
    let words = words(text);
    let mut lines: Vec<String> = Vec::new();
    let mut push = |source: &str, resolved: String| {
        let line = format!("- \"{source}\" -> {resolved} (inferred from context)");
        if !lines.contains(&line) {
            lines.push(line);
        }
    };

    let mut index = 0;
    while index < words.len() {
        if let Some((len, date)) = absolute_at(text, &words, index) {
            base = Some(date);
            index += len;
            continue;
        }
        if let Some((len, month, day)) = yearless_at(&words, index) {
            if let Some(base) = base {
                let source = &text[words[index].start..words[index + len - 1].end];
                let resolved = match day {
                    Some(day) => NaiveDate::from_ymd_opt(base.year(), month, day)
                        .map(|date| date.format("%Y-%m-%d").to_string()),
                    None => Some(format!("{:04}-{month:02}", base.year())),
                };
                if let Some(resolved) = resolved {
                    push(source, resolved);
                }
            }
            index += len;
            continue;
        }
        if let Some((len, amount, unit)) = relative_at(&words, index) {
            if let Some(date) = base.and_then(|base| offset(base, amount, unit)) {
                let source = &text[words[index].start..words[index + len - 1].end];
                push(source, date.format("%Y-%m-%d").to_string());
            }
            index += len;
            continue;
        }
        if is_filler(words[index].text) || !words[index].text.is_empty() {
            index += 1;
        }
    }
    (lines, base)
}

/// Hint lines for every chunk of one document, in document order.
///
/// A pure function of the texts: the rolling base advances from one chunk to
/// the next as a local, so the same texts always yield the same hints however
/// the chunks are later batched for extraction. Dates never carry across
/// documents — call once per document.
pub fn document_temporal_hints<S: AsRef<str>>(chunk_texts: &[S]) -> Vec<Vec<String>> {
    let mut base = None;
    chunk_texts
        .iter()
        .map(|text| {
            let (lines, next) = hint_lines(text.as_ref(), base);
            base = next;
            lines
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    // Mirrors Python's cognee/tests/unit/modules/engine/test_temporal_hints.py.

    #[test]
    fn hint_lines_rolls_base_and_marks_inferred() {
        let (lines, base) = hint_lines(
            "On 26 April 1986 the reactor exploded. The following night of 27 April, engineers worked.",
            None,
        );
        assert_eq!(base, NaiveDate::from_ymd_opt(1986, 4, 26));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("1986-04-27"));
        assert!(lines[0].contains("inferred from context"));
        assert!(lines[0].contains("\"27 April\""));
    }

    #[test]
    fn without_a_stated_year_nothing_is_hinted() {
        let (lines, base) = hint_lines("The following night of 27 April, engineers worked.", None);
        assert!(lines.is_empty());
        assert_eq!(base, None);
    }

    #[test]
    fn scores_durations_and_money_are_ignored() {
        let (lines, base) = hint_lines(
            "On 26 April 1986 they won 9-1. The broadcast lasted two minutes and cost $1990.",
            None,
        );
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(base, NaiveDate::from_ymd_opt(1986, 4, 26));
    }

    #[test]
    fn lower_case_may_and_decade_are_not_months() {
        let (lines, _) = hint_lines(
            "In 1961 they said: before this decade is out, they may well ask.",
            None,
        );
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn month_only_relative_offsets_and_ordinals() {
        let (lines, _) = hint_lines(
            "Founded on 3 March 2020. In May it shipped; four weeks later it failed. On Sept 12th it closed.",
            None,
        );
        assert_eq!(
            lines,
            vec![
                "- \"May\" -> 2020-05 (inferred from context)".to_string(),
                "- \"four weeks later\" -> 2020-03-31 (inferred from context)".to_string(),
                "- \"Sept 12th\" -> 2020-09-12 (inferred from context)".to_string(),
            ]
        );
    }

    #[test]
    fn a_stated_year_in_any_absolute_shape_moves_the_base() {
        let (lines, base) = hint_lines("Released 2021-06-01. Patched on 5 July.", None);
        assert_eq!(base, NaiveDate::from_ymd_opt(2021, 6, 1));
        assert_eq!(
            lines,
            vec!["- \"5 July\" -> 2021-07-05 (inferred from context)"]
        );
    }

    #[test]
    fn a_bare_month_needs_a_word_that_introduces_it() {
        let (lines, _) = hint_lines(
            "Founded in 2020. May we never repeat it. April joined the team. By June it grew.",
            None,
        );
        assert_eq!(lines, vec!["- \"June\" -> 2020-06 (inferred from context)"]);
    }

    const FIRST: &str = "On 26 April 1986 the reactor exploded.";
    const SECOND: &str = "The following night of 27 April, engineers worked.";

    #[test]
    fn document_hints_roll_the_base_in_order_and_are_a_pure_function() {
        let hints = document_temporal_hints(&[FIRST, SECOND]);
        assert!(hints[0].is_empty());
        assert_eq!(hints[1].len(), 1);
        assert!(hints[1][0].contains("1986-04-27"));
        assert_eq!(document_temporal_hints(&[FIRST, SECOND]), hints);
        // The base only flows forward, and never crosses documents.
        assert_eq!(
            document_temporal_hints(&[SECOND, FIRST]),
            vec![Vec::<String>::new(), vec![]]
        );
        assert_eq!(
            document_temporal_hints(&[SECOND]),
            vec![Vec::<String>::new()]
        );
    }
}
