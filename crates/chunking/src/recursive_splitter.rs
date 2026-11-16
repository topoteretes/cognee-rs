//! Port of langchain's `RecursiveCharacterTextSplitter`.
//!
//! Python's `LangchainChunker` (`cognee/modules/chunking/LangchainChunker.py`)
//! is a thin wrapper over
//! `langchain_text_splitters.RecursiveCharacterTextSplitter(chunk_size=1024,
//! chunk_overlap=10, length_function=lambda t: len(t.split()))`. This module
//! reproduces that splitter (`langchain-text-splitters` 1.1.3,
//! `character.py` / `base.py`) with the defaults the chunker relies on:
//!
//! * separators `["\n\n", "\n", " ", ""]`, matched literally
//!   (`is_separator_regex=False`);
//! * `keep_separator=True`, i.e. `"start"` — each separator is glued to the
//!   front of the piece that follows it, and pieces are merged with `""`;
//! * `strip_whitespace=True` — every emitted chunk is stripped, and a chunk
//!   that strips to nothing is dropped.
//!
//! Length is measured by a caller-supplied function. [`word_length`] is the
//! one `LangchainChunker` uses (`len(text.split())`).

use crate::error::ChunkingError;

/// `len(text.split())` — the length function `LangchainChunker` passes to the
/// splitter. Python's `str.split()` and Rust's `split_whitespace` both split on
/// runs of Unicode whitespace and drop empty fields.
pub fn word_length(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The default separators of `RecursiveCharacterTextSplitter`.
pub const DEFAULT_SEPARATORS: [&str; 4] = ["\n\n", "\n", " ", ""];

/// A recursive character splitter, ported from langchain.
///
/// Splits on the first separator that occurs in the text, merges the pieces
/// back into chunks of at most `chunk_size` (by `length`), carrying up to
/// `chunk_overlap` of the previous chunk's tail into the next, and recurses
/// with the remaining separators into any piece that is still too long.
pub struct RecursiveCharacterSplitter<L: Fn(&str) -> usize> {
    chunk_size: usize,
    chunk_overlap: usize,
    separators: Vec<String>,
    length: L,
}

impl<L: Fn(&str) -> usize> RecursiveCharacterSplitter<L> {
    /// Build a splitter with langchain's default separators.
    ///
    /// Rejects the configurations langchain's `TextSplitter.__init__` rejects:
    /// `chunk_size == 0` and `chunk_overlap > chunk_size`. (A negative overlap,
    /// its third check, is unrepresentable here.)
    pub fn new(chunk_size: usize, chunk_overlap: usize, length: L) -> Result<Self, ChunkingError> {
        if chunk_size == 0 {
            return Err(ChunkingError::InvalidConfig(format!(
                "chunk_size must be > 0, got {chunk_size}"
            )));
        }
        if chunk_overlap > chunk_size {
            return Err(ChunkingError::InvalidConfig(format!(
                "Got a larger chunk overlap ({chunk_overlap}) than chunk size \
                 ({chunk_size}), should be smaller."
            )));
        }
        Ok(Self {
            chunk_size,
            chunk_overlap,
            separators: DEFAULT_SEPARATORS.iter().map(|s| (*s).to_owned()).collect(),
            length,
        })
    }

    /// Split `text` into chunks (langchain's `split_text`).
    pub fn split_text(&self, text: &str) -> Vec<String> {
        self.split_recursive(text, &self.separators)
    }

    /// langchain's `_split_text`.
    fn split_recursive(&self, text: &str, separators: &[String]) -> Vec<String> {
        let mut final_chunks = Vec::new();

        // Pick the first separator present in the text; `""` always matches
        // and ends the search with no finer separators left.
        let mut separator: &str = separators.last().map(String::as_str).unwrap_or_default();
        let mut new_separators: &[String] = &[];
        for (i, s) in separators.iter().enumerate() {
            if s.is_empty() {
                separator = s;
                break;
            }
            if text.contains(s.as_str()) {
                separator = s;
                new_separators = &separators[i + 1..];
                break;
            }
        }

        let splits = split_keeping_separator(text, separator);

        // With `keep_separator`, the separator already rides on the pieces, so
        // they are merged with "".
        let mut good_splits: Vec<String> = Vec::new();
        for s in splits {
            if (self.length)(&s) < self.chunk_size {
                good_splits.push(s);
            } else {
                if !good_splits.is_empty() {
                    final_chunks.extend(self.merge_splits(&good_splits, ""));
                    good_splits.clear();
                }
                if new_separators.is_empty() {
                    final_chunks.push(s);
                } else {
                    final_chunks.extend(self.split_recursive(&s, new_separators));
                }
            }
        }
        if !good_splits.is_empty() {
            final_chunks.extend(self.merge_splits(&good_splits, ""));
        }
        final_chunks
    }

    /// langchain's `_merge_splits`: greedily pack `splits` into chunks of at
    /// most `chunk_size`, keeping a tail of at most `chunk_overlap` from one
    /// chunk as the head of the next.
    fn merge_splits(&self, splits: &[String], separator: &str) -> Vec<String> {
        let separator_len = (self.length)(separator);
        let mut docs = Vec::new();
        let mut current: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
        let mut total: usize = 0;

        for d in splits {
            let len = (self.length)(d);
            let sep_if_any = |cur: &std::collections::VecDeque<&str>| {
                if cur.is_empty() { 0 } else { separator_len }
            };
            if total + len + sep_if_any(&current) > self.chunk_size {
                if total > self.chunk_size {
                    tracing::warn!(
                        size = total,
                        chunk_size = self.chunk_size,
                        "created a chunk longer than the specified chunk size"
                    );
                }
                if !current.is_empty() {
                    if let Some(doc) = join_docs(current.iter().copied(), separator) {
                        docs.push(doc);
                    }
                    // Pop from the front while the kept tail is longer than the
                    // overlap, or while the next piece still would not fit.
                    while let Some(front) = current.front().copied() {
                        let too_long = total > self.chunk_overlap;
                        let no_room =
                            total + len + sep_if_any(&current) > self.chunk_size && total > 0;
                        if !(too_long || no_room) {
                            break;
                        }
                        let popped_sep = if current.len() > 1 { separator_len } else { 0 };
                        total = total.saturating_sub((self.length)(front) + popped_sep);
                        current.pop_front();
                    }
                }
            }
            current.push_back(d);
            total += len + if current.len() > 1 { separator_len } else { 0 };
        }
        if let Some(doc) = join_docs(current.iter().copied(), separator) {
            docs.push(doc);
        }
        docs
    }
}

/// langchain's `_join_docs` with `strip_whitespace=True`.
fn join_docs<'a>(docs: impl Iterator<Item = &'a str>, separator: &str) -> Option<String> {
    let joined = docs.collect::<Vec<_>>().join(separator);
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// langchain's `_split_text_with_regex` for a literal separator and
/// `keep_separator="start"`: `"a\nb\nc"` on `"\n"` gives `["a", "\nb", "\nc"]`.
/// An empty separator splits into single characters. Empty pieces are dropped.
fn split_keeping_separator(text: &str, separator: &str) -> Vec<String> {
    let pieces: Vec<String> = if separator.is_empty() {
        text.chars().map(String::from).collect()
    } else {
        let mut parts = text.split(separator);
        let mut out = Vec::new();
        if let Some(first) = parts.next() {
            out.push(first.to_owned());
        }
        out.extend(parts.map(|p| format!("{separator}{p}")));
        out
    };
    pieces.into_iter().filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn splitter(size: usize, overlap: usize) -> RecursiveCharacterSplitter<fn(&str) -> usize> {
        RecursiveCharacterSplitter::new(size, overlap, word_length as fn(&str) -> usize).unwrap()
    }

    fn char_splitter(size: usize, overlap: usize) -> RecursiveCharacterSplitter<fn(&str) -> usize> {
        RecursiveCharacterSplitter::new(
            size,
            overlap,
            (|s: &str| s.chars().count()) as fn(&str) -> usize,
        )
        .unwrap()
    }

    #[test]
    fn rejects_langchain_invalid_configs() {
        assert!(RecursiveCharacterSplitter::new(0, 0, word_length).is_err());
        assert!(RecursiveCharacterSplitter::new(5, 6, word_length).is_err());
        assert!(RecursiveCharacterSplitter::new(5, 5, word_length).is_ok());
    }

    #[test]
    fn split_keeps_separator_at_start() {
        assert_eq!(
            split_keeping_separator("a\nb\nc", "\n"),
            vec!["a", "\nb", "\nc"]
        );
        assert_eq!(split_keeping_separator("\na", "\n"), vec!["\na"]);
        assert_eq!(split_keeping_separator("ab", ""), vec!["a", "b"]);
    }

    #[test]
    fn short_text_is_one_stripped_chunk() {
        assert_eq!(
            splitter(1024, 10).split_text("  hello world \n"),
            vec!["hello world"]
        );
    }

    #[test]
    fn empty_and_whitespace_text_yield_nothing() {
        assert!(splitter(1024, 10).split_text("").is_empty());
        assert!(splitter(1024, 10).split_text(" \n\n \n").is_empty());
    }

    /// Outputs pinned from `langchain-text-splitters` 1.1.3
    /// (`RecursiveCharacterTextSplitter(chunk_size=…, chunk_overlap=…,
    /// length_function=len)`), the canonical example in langchain's tests.
    #[test]
    fn matches_langchain_character_length_example() {
        let text = "Hi.\n\nI'm Harrison.\n\nHow? Are? You?\nOkay then f f f f.\nThis is a weird text to write, but gotta test the splittingggg some how.\n\nBye!\n\n-H.";
        let got = char_splitter(10, 1).split_text(text);
        let expected = vec![
            "Hi.",
            "I'm",
            "Harrison.",
            "How? Are?",
            "You?",
            "Okay then",
            "f f f f.",
            "This is a",
            "weird",
            "text to",
            "write,",
            "but gotta",
            "test the",
            "splitting",
            "gggg",
            "some how.",
            "Bye!",
            "-H.",
        ];
        assert_eq!(got, expected);
    }

    #[test]
    fn word_length_chunks_respect_size_and_overlap() {
        // 30 words in one paragraph, 10 words per chunk, 2 words overlap.
        let words: Vec<String> = (0..30).map(|i| format!("w{i}")).collect();
        let text = words.join(" ");
        let chunks = splitter(10, 2).split_text(&text);
        assert!(chunks.len() > 3);
        for c in &chunks {
            assert!(word_length(c) <= 10, "chunk too long: {c}");
        }
        // Consecutive chunks share a tail/head word.
        for pair in chunks.windows(2) {
            let last = pair[0].split_whitespace().last().unwrap();
            assert!(
                pair[1].split_whitespace().any(|w| w == last),
                "no overlap between {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
        // All content survives, in order.
        assert!(chunks[0].starts_with("w0 "));
        assert!(chunks.last().unwrap().ends_with("w29"));
    }

    #[test]
    fn paragraphs_are_preferred_split_points() {
        let text = "one two three\n\nfour five six\n\nseven eight nine";
        let chunks = splitter(4, 0).split_text(text);
        assert_eq!(
            chunks,
            vec!["one two three", "four five six", "seven eight nine"]
        );
    }

    #[test]
    fn unsplittable_long_word_is_emitted_alone() {
        let chunks = char_splitter(3, 0).split_text("abcdef");
        assert_eq!(chunks, vec!["abc", "def"]);
    }
}
