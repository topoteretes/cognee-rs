//! Word-level text chunker.
//!
//! Splits text into words and sentence/paragraph endings, preserving whitespace.
//! Outputs can be concatenated with "" to reconstruct the original input (isomorphism).
//!
//! Port of Python `cognee.tasks.chunks.chunk_by_word`.

/// The type of a word-level chunk.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WordType {
    Word,
    SentenceEnd,
    ParagraphEnd,
}

/// A single word-level chunk: a borrowed slice of the input text and its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordChunk<'a> {
    pub text: &'a str,
    pub word_type: WordType,
}

fn is_sentence_ending(c: char) -> bool {
    matches!(c, '.' | ';' | '!' | '?' | '…' | '。' | '！' | '？')
}

fn is_paragraph_ending(c: char) -> bool {
    matches!(c, '\n' | '\r')
}

/// Returns word-level chunks from the input text as borrowed slices.
///
/// The algorithm:
/// - A space terminates the current word (space is included with the preceding word).
/// - Sentence-ending punctuation (`.;!?…。！？`) triggers a sentence_end or paragraph_end.
///   - Trailing spaces after punctuation are consumed into the chunk.
///   - If the next non-space character is a newline, it's a paragraph_end; otherwise sentence_end.
/// - Remaining text at end is yielded as a word.
pub fn chunk_by_word(data: &str) -> Vec<WordChunk<'_>> {
    let mut result = Vec::new();
    let mut chunk_start = 0usize;
    let mut iter = data.char_indices().peekable();

    while let Some((byte_pos, ch)) = iter.next() {
        if ch == ' ' {
            let end = byte_pos + 1; // space is always 1 byte
            result.push(WordChunk {
                text: &data[chunk_start..end],
                word_type: WordType::Word,
            });
            chunk_start = end;
            continue;
        }

        if is_sentence_ending(ch) {
            // Consume trailing spaces via peek + next
            let mut end = byte_pos + ch.len_utf8();
            while let Some(&(next_bp, ' ')) = iter.peek() {
                end = next_bp + 1;
                iter.next();
            }

            let is_para_end = iter.peek().is_some_and(|&(_, c)| is_paragraph_ending(c));
            result.push(WordChunk {
                text: &data[chunk_start..end],
                word_type: if is_para_end {
                    WordType::ParagraphEnd
                } else {
                    WordType::SentenceEnd
                },
            });
            chunk_start = end;
            continue;
        }
    }

    if chunk_start < data.len() {
        result.push(WordChunk {
            text: &data[chunk_start..],
            word_type: WordType::Word,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        assert!(chunk_by_word("").is_empty());
    }

    #[test]
    fn single_word() {
        let chunks = chunk_by_word("hello");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello");
        assert_eq!(chunks[0].word_type, WordType::Word);
    }

    #[test]
    fn two_words() {
        let chunks = chunk_by_word("hello world");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "hello ");
        assert_eq!(chunks[0].word_type, WordType::Word);
        assert_eq!(chunks[1].text, "world");
        assert_eq!(chunks[1].word_type, WordType::Word);
    }

    #[test]
    fn sentence_end() {
        let chunks = chunk_by_word("Hello world. Foo");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Hello ");
        assert_eq!(chunks[0].word_type, WordType::Word);
        assert_eq!(chunks[1].text, "world. ");
        assert_eq!(chunks[1].word_type, WordType::SentenceEnd);
        assert_eq!(chunks[2].text, "Foo");
        assert_eq!(chunks[2].word_type, WordType::Word);
    }

    #[test]
    fn paragraph_end() {
        // "Hello." is a paragraph_end (period followed by \n)
        // The \n is NOT consumed — it becomes part of the next word "\nWorld"
        let chunks = chunk_by_word("Hello.\nWorld");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Hello.");
        assert_eq!(chunks[0].word_type, WordType::ParagraphEnd);
        assert_eq!(chunks[1].text, "\nWorld");
        assert_eq!(chunks[1].word_type, WordType::Word);
    }

    #[test]
    fn paragraph_end_with_space() {
        // Trailing space after period is consumed into the sentence ending
        let chunks = chunk_by_word("Hello. \nWorld");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Hello. ");
        assert_eq!(chunks[0].word_type, WordType::ParagraphEnd);
        assert_eq!(chunks[1].text, "\nWorld");
        assert_eq!(chunks[1].word_type, WordType::Word);
    }

    #[test]
    fn multiple_punctuation_types() {
        let chunks = chunk_by_word("What? Yes! Really; ok.");
        let types: Vec<_> = chunks.iter().map(|c| &c.word_type).collect();
        assert_eq!(
            types,
            vec![
                &WordType::SentenceEnd, // "What? "
                &WordType::SentenceEnd, // "Yes! "
                &WordType::SentenceEnd, // "Really; "
                &WordType::SentenceEnd, // "ok."
            ]
        );
    }

    #[test]
    fn isomorphism() {
        let input = "This is a test. It has multiple sentences.\nAnd paragraphs! Really? Yes.";
        let chunks = chunk_by_word(input);
        let reconstructed: String = chunks.iter().map(|c| c.text).collect();
        assert_eq!(reconstructed, input);
    }

    #[test]
    fn isomorphism_with_newlines() {
        let input = "First paragraph.\nSecond paragraph.\nThird.";
        let chunks = chunk_by_word(input);
        let reconstructed: String = chunks.iter().map(|c| c.text).collect();
        assert_eq!(reconstructed, input);
    }

    #[test]
    fn isomorphism_parametrized() {
        use crate::test_inputs::ALL_INPUTS;

        for &(name, input) in ALL_INPUTS {
            let chunks = chunk_by_word(input);
            let reconstructed: String = chunks.iter().map(|c| c.text).collect();
            assert_eq!(reconstructed, input, "isomorphism failed for '{name}'");
        }
    }

    #[test]
    fn no_internal_spaces_in_word_chunks() {
        use crate::test_inputs::ALL_INPUTS;

        for &(name, input) in ALL_INPUTS {
            if input.is_empty() {
                continue;
            }
            let chunks = chunk_by_word(input);
            for (i, chunk) in chunks.iter().enumerate() {
                assert!(
                    !chunk.text.trim().contains(' '),
                    "chunk {i} in '{name}' has internal space: {:?}",
                    chunk.text
                );
            }
        }
    }
}
