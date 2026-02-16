//! Sentence-level text chunker.
//!
//! Aggregates word-level chunks into sentences, tracking paragraph boundaries
//! and token counts.
//!
//! Port of Python `cognee.tasks.chunks.chunk_by_sentence`.

use uuid::Uuid;

use crate::chunk_by_word::{WordType, chunk_by_word};
use crate::cut_type::CutType;
use crate::token_counter::TokenCounter;

/// A sentence-level chunk with metadata. Borrows text from the input.
#[derive(Debug, Clone)]
pub struct SentenceChunk<'a> {
    /// Unique paragraph identifier. Changes on paragraph boundaries.
    pub paragraph_id: Uuid,
    /// The sentence text, borrowed from the input.
    pub text: &'a str,
    /// Token count of the sentence (via TokenCounter).
    pub size: usize,
    /// How the sentence boundary was determined.
    pub cut_type: CutType,
}

fn word_type_to_cut_type(wt: WordType) -> CutType {
    match wt {
        WordType::ParagraphEnd => CutType::ParagraphEnd,
        WordType::SentenceEnd => CutType::SentenceEnd,
        WordType::Word => CutType::Word,
    }
}

/// Computes the byte offset of a `&str` slice relative to the start of `base`.
fn offset_in(base: &str, slice: &str) -> usize {
    slice.as_ptr() as usize - base.as_ptr() as usize
}

/// Splits text into sentences based on word-level tokenization.
///
/// - `data`: the input text
/// - `maximum_size`: optional max token count per sentence. If a sentence would
///   exceed this, it is yielded early and the overflowing word starts a new one.
/// - `counter`: token counter implementation
pub fn chunk_by_sentence<'a, C: TokenCounter>(
    data: &'a str,
    maximum_size: Option<usize>,
    counter: &C,
) -> Vec<SentenceChunk<'a>> {
    let words = chunk_by_word(data);
    let mut result = Vec::new();
    let mut paragraph_id = Uuid::new_v4();
    let mut sentence_size: usize = 0;
    let mut word_type_state = WordType::Word;
    // Track the byte range of the current sentence in `data`.
    let mut sentence_start: Option<usize> = None;
    let mut sentence_end: usize = 0;

    for word_chunk in &words {
        let word = word_chunk.text;
        let word_type = word_chunk.word_type;
        let word_size = counter.count_tokens(word);

        let word_start_byte = offset_in(data, word);
        let word_end_byte = word_start_byte + word.len();

        // Update word_type_state: for sentence/paragraph ends, take directly.
        // For words, only update if the word contains alphabetic characters.
        match word_type {
            WordType::ParagraphEnd | WordType::SentenceEnd => {
                word_type_state = word_type;
            }
            WordType::Word => {
                if word.chars().any(|c| c.is_alphabetic()) {
                    word_type_state = word_type;
                }
            }
        }

        // Check overflow
        if let Some(max) = maximum_size
            && sentence_size + word_size > max
            && sentence_start.is_some()
        {
            result.push(SentenceChunk {
                paragraph_id,
                text: &data[sentence_start.unwrap()..sentence_end],
                size: sentence_size,
                cut_type: word_type_to_cut_type(word_type_state),
            });
            sentence_start = Some(word_start_byte);
            sentence_end = word_end_byte;
            sentence_size = word_size;
            continue;
        }

        if matches!(word_type, WordType::ParagraphEnd | WordType::SentenceEnd) {
            if sentence_start.is_none() {
                sentence_start = Some(word_start_byte);
            }
            sentence_end = word_end_byte;
            sentence_size += word_size;

            if word_type == WordType::ParagraphEnd {
                paragraph_id = Uuid::new_v4();
            }

            result.push(SentenceChunk {
                paragraph_id,
                text: &data[sentence_start.unwrap()..sentence_end],
                size: sentence_size,
                cut_type: word_type_to_cut_type(word_type_state),
            });
            sentence_start = None;
            sentence_size = 0;
        } else {
            if sentence_start.is_none() {
                sentence_start = Some(word_start_byte);
            }
            sentence_end = word_end_byte;
            sentence_size += word_size;
        }
    }

    if let Some(start) = sentence_start {
        let cut_type = if word_type_state == WordType::Word {
            CutType::SentenceCut
        } else {
            word_type_to_cut_type(word_type_state)
        };
        result.push(SentenceChunk {
            paragraph_id,
            text: &data[start..sentence_end],
            size: sentence_size,
            cut_type,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_counter::WordCounter;

    #[test]
    fn empty_input() {
        let chunks = chunk_by_sentence("", None, &WordCounter);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_sentence() {
        let chunks = chunk_by_sentence("Hello world.", None, &WordCounter);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello world.");
        assert_eq!(chunks[0].size, 2);
        assert_eq!(chunks[0].cut_type, CutType::SentenceEnd);
    }

    #[test]
    fn two_sentences_same_paragraph() {
        let chunks = chunk_by_sentence("Hello world. Foo bar.", None, &WordCounter);
        assert_eq!(chunks.len(), 2);
        // Same paragraph_id for both
        assert_eq!(chunks[0].paragraph_id, chunks[1].paragraph_id);
    }

    #[test]
    fn paragraph_boundary_new_id() {
        // In Python, paragraph_id is updated on paragraph_end BEFORE yielding,
        // so the sentence with paragraph_end gets the NEW id, and subsequent
        // sentences share that id until the next paragraph_end.
        // Two separate paragraphs should have different IDs:
        let chunks = chunk_by_sentence(
            "First paragraph.\nSecond paragraph.\nThird.",
            None,
            &WordCounter,
        );
        assert_eq!(chunks.len(), 3);
        // First paragraph_end triggers new id for chunks[0]
        // Second paragraph_end triggers another new id for chunks[1]
        // chunks[0] and chunks[1] should differ (different paragraph_ends)
        assert_ne!(chunks[0].paragraph_id, chunks[1].paragraph_id);
    }

    #[test]
    fn sentence_cut_no_punctuation() {
        let chunks = chunk_by_sentence("Hello world", None, &WordCounter);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].cut_type, CutType::SentenceCut);
    }

    #[test]
    fn maximum_size_overflow() {
        // max 2 words per sentence
        let chunks = chunk_by_sentence("one two three four", Some(2), &WordCounter);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "one two ");
        assert_eq!(chunks[0].size, 2);
        assert_eq!(chunks[1].text, "three four");
        assert_eq!(chunks[1].size, 2);
    }

    #[test]
    fn token_counting_matches_word_count() {
        let chunks = chunk_by_sentence("This is a test sentence.", None, &WordCounter);
        assert_eq!(chunks[0].size, 5);
    }
}
