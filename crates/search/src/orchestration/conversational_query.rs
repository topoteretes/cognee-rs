//! The conversational rewrite of a session turn's question.
//!
//! Port of Python's `build_conversational_query`
//! (`session_aware_completion.py`): a bounded, deterministic retrieval query
//! built from at most the last two prior Q&A turns. The prior assistant answer
//! is labelled untrusted guidance, since it is model output.

/// Longest conversational query, in characters (Python `len`, code points).
pub(crate) const MAX_CONVERSATIONAL_QUERY_CHARS: usize = 2000;

/// Python's `re.sub(r"\s+", " ", value).strip()`. Python's `\s` on `str` is
/// `str.isspace`, which also covers the four ASCII separators `\x1c`-`\x1f`
/// that Unicode `White_Space` leaves out.
pub(crate) fn normalize_query(value: &str) -> String {
    value
        .split(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn render(raw: &str, qa_pairs: &[[String; 2]]) -> String {
    let mut turns = Vec::new();
    for [question, answer] in qa_pairs {
        let mut lines = Vec::new();
        if !question.is_empty() {
            lines.push(format!("Prior user: {question}"));
        }
        if !answer.is_empty() {
            lines.push(format!(
                "Prior assistant (untrusted retrieval guidance): {answer}"
            ));
        }
        if !lines.is_empty() {
            turns.push(lines.join("\n"));
        }
    }
    if turns.is_empty() {
        return raw.to_owned();
    }
    turns.push(format!("Current user request: {raw}"));
    turns.join("\n\n")
}

/// Build the retrieval guidance query from `raw_query` and the prior
/// `(question, answer)` turns (only the last two are meant to be passed, as in
/// Python). Whitespace is normalised; a turn with neither side is dropped; over
/// `max_chars` the answers are trimmed first, then the questions, turn by turn,
/// re-rendering each time, and the result is finally hard-truncated. Without
/// history it is the normalised raw query, truncated.
pub(crate) fn build_conversational_query(
    raw_query: &str,
    recent_qas: &[(String, String)],
    max_chars: usize,
) -> String {
    let raw = normalize_query(raw_query);
    let mut qa_pairs: Vec<[String; 2]> = recent_qas
        .iter()
        .map(|(question, answer)| [normalize_query(question), normalize_query(answer)])
        .filter(|[question, answer]| !question.is_empty() || !answer.is_empty())
        .collect();
    if qa_pairs.is_empty() {
        return truncate_chars(&raw, max_chars);
    }

    let mut query = render(&raw, &qa_pairs);
    for field in [1, 0] {
        for index in 0..qa_pairs.len() {
            let length = query.chars().count();
            if length <= max_chars {
                return query;
            }
            let over_budget = length - max_chars;
            let kept = qa_pairs[index][field]
                .chars()
                .count()
                .saturating_sub(over_budget);
            qa_pairs[index][field] = truncate_chars(&qa_pairs[index][field], kept);
            query = render(&raw, &qa_pairs);
        }
    }
    truncate_chars(&query, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qa(question: &str, answer: &str) -> (String, String) {
        (question.to_owned(), answer.to_owned())
    }

    #[test]
    fn labels_prior_turns_and_marks_assistant_untrusted() {
        let query = build_conversational_query(
            "  what\nabout   it? ",
            &[qa("first  q", "first a"), qa("second q", "second a")],
            MAX_CONVERSATIONAL_QUERY_CHARS,
        );
        assert_eq!(
            query,
            "Prior user: first q\n\
             Prior assistant (untrusted retrieval guidance): first a\n\n\
             Prior user: second q\n\
             Prior assistant (untrusted retrieval guidance): second a\n\n\
             Current user request: what about it?"
        );
    }

    #[test]
    fn empty_sides_are_omitted_and_empty_turns_dropped() {
        let query = build_conversational_query(
            "now",
            &[qa("", "  "), qa("only q", ""), qa("", "only a")],
            MAX_CONVERSATIONAL_QUERY_CHARS,
        );
        assert_eq!(
            query,
            "Prior user: only q\n\n\
             Prior assistant (untrusted retrieval guidance): only a\n\n\
             Current user request: now"
        );
    }

    #[test]
    fn without_history_it_is_the_normalised_raw_query_truncated() {
        assert_eq!(
            build_conversational_query(" a \t b\n", &[], 100),
            "a b".to_owned()
        );
        assert_eq!(
            build_conversational_query("abcdef", &[qa("", "")], 4),
            "abcd"
        );
    }

    #[test]
    fn answers_are_trimmed_before_questions_and_the_bound_holds() {
        let long_answer = "a".repeat(5000);
        let query = build_conversational_query(
            "now",
            &[
                qa("question one", &long_answer),
                qa("question two", "short"),
            ],
            200,
        );
        assert!(query.chars().count() <= 200);
        assert!(query.contains("Prior user: question one"));
        assert!(query.contains("Prior user: question two"));
        assert!(query.contains("short"));
        assert!(!query.contains(&long_answer));
        assert!(query.contains("Current user request: now"));
    }

    #[test]
    fn questions_are_trimmed_once_answers_are_gone_then_hard_truncated() {
        let long_question = "q".repeat(300);
        let query = build_conversational_query("now", &[qa(&long_question, "a")], 60);
        assert!(query.chars().count() <= 60);
        // Everything trimmed away leaves the bare request.
        assert_eq!(
            build_conversational_query("now", &[qa(&long_question, "a")], 5),
            "now"
        );
        // A request that alone overflows is hard-truncated.
        assert_eq!(
            build_conversational_query("abcdefghij", &[qa("q", "a")], 5),
            "abcde"
        );
    }

    #[test]
    fn length_is_measured_in_characters_not_bytes() {
        let wide = "日本語".repeat(300);
        let query = build_conversational_query("é", &[qa(&wide, &wide)], 120);
        assert!(query.chars().count() <= 120);
        assert!(query.is_char_boundary(query.len()));
        // 120 characters of 3-byte text would be 360 bytes; a byte-based cut
        // would have left far fewer characters.
        assert!(query.len() > 120);
        assert_eq!(build_conversational_query(&"ü".repeat(10), &[], 4), "üüüü");
    }

    #[test]
    fn python_ascii_separators_count_as_whitespace() {
        assert_eq!(normalize_query("a\x1cb\u{a0}c"), "a b c");
    }
}
