//! Paragraph-level text chunker.
//!
//! Batches sentences into paragraph-sized chunks, respecting a maximum token
//! count. Supports both batch mode (accumulate across paragraphs) and
//! non-batch mode (yield at each paragraph boundary).

use cognee_utils::NAMESPACE_OID;
use uuid::Uuid;

use crate::chunk_by_sentence::chunk_by_sentence;
use crate::cut_type::CutType;
use crate::token_counter::TokenCounter;

/// A paragraph-level chunk with metadata. Borrows text from the input.
#[derive(Debug, Clone)]
pub struct ParagraphChunk<'a> {
    /// The accumulated text, borrowed from the input.
    pub text: &'a str,
    /// Token count of the chunk.
    pub chunk_size: usize,
    /// Deterministic ID: uuid5(NAMESPACE_OID, text).
    pub chunk_id: Uuid,
    /// Paragraph IDs from the sentences that compose this chunk.
    pub paragraph_ids: Vec<Uuid>,
    /// Sequential chunk index.
    pub chunk_index: usize,
    /// How the chunk boundary was determined.
    pub cut_type: CutType,
}

/// Chunks text by paragraph, optionally batching across paragraph boundaries.
///
/// - `data`: input text
/// - `max_chunk_size`: maximum token count per chunk
/// - `batch_paragraphs`: if true, accumulates sentences across paragraph
///   boundaries until overflow. If false, yields at each paragraph boundary.
/// - `counter`: token counter implementation
pub fn chunk_by_paragraph<'a, C: TokenCounter>(
    data: &'a str,
    max_chunk_size: usize,
    batch_paragraphs: bool,
    counter: &C,
) -> Vec<ParagraphChunk<'a>> {
    let sentences = chunk_by_sentence(data, Some(max_chunk_size), counter);
    let mut result = Vec::new();
    let mut chunk_index: usize = 0;
    let mut paragraph_ids: Vec<Uuid> = Vec::new();
    let mut last_cut_type = CutType::SentenceCut;
    let mut current_chunk_size: usize = 0;
    // Track the byte range of the current chunk in `data`.
    let mut chunk_start: Option<usize> = None;
    let mut chunk_end: usize = 0;

    for sentence in &sentences {
        let sent_start = sentence.text.as_ptr() as usize - data.as_ptr() as usize;
        let sent_end = sent_start + sentence.text.len();

        // Overflow: yield current chunk and start fresh
        if current_chunk_size > 0 && (current_chunk_size + sentence.size > max_chunk_size) {
            let text = &data[chunk_start.expect("chunk_start is Some because current_chunk_size > 0 only after a sentence was accumulated")..chunk_end];
            result.push(ParagraphChunk {
                text,
                chunk_size: current_chunk_size,
                chunk_id: Uuid::new_v5(&NAMESPACE_OID, text.as_bytes()),
                paragraph_ids: std::mem::take(&mut paragraph_ids),
                chunk_index,
                cut_type: last_cut_type.clone(),
            });
            current_chunk_size = 0;
            chunk_start = None;
            chunk_index += 1;
        }

        paragraph_ids.push(sentence.paragraph_id);
        if chunk_start.is_none() {
            chunk_start = Some(sent_start);
        }
        chunk_end = sent_end;
        current_chunk_size += sentence.size;

        // Non-batch mode: yield at paragraph boundaries
        if !batch_paragraphs
            && matches!(
                sentence.cut_type,
                CutType::ParagraphEnd | CutType::SentenceCut
            )
        {
            let text = &data[chunk_start.expect("chunk_start is Some because it was set above before this emit branch is reached")..chunk_end];
            result.push(ParagraphChunk {
                text,
                chunk_size: current_chunk_size,
                chunk_id: Uuid::new_v5(&NAMESPACE_OID, text.as_bytes()),
                paragraph_ids: std::mem::take(&mut paragraph_ids),
                chunk_index,
                cut_type: sentence.cut_type.clone(),
            });
            current_chunk_size = 0;
            chunk_start = None;
            chunk_index += 1;
        }

        last_cut_type = sentence.cut_type.clone();
    }

    // Yield remaining text
    if let Some(start) = chunk_start {
        let final_cut_type = if last_cut_type == CutType::Word {
            CutType::SentenceCut
        } else {
            last_cut_type
        };
        let text = &data[start..chunk_end];
        result.push(ParagraphChunk {
            chunk_id: Uuid::new_v5(&NAMESPACE_OID, text.as_bytes()),
            text,
            chunk_size: current_chunk_size,
            paragraph_ids,
            chunk_index,
            cut_type: final_cut_type,
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
        let chunks = chunk_by_paragraph("", 10, true, &WordCounter);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_short_paragraph() {
        let chunks = chunk_by_paragraph("Hello world.", 100, true, &WordCounter);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello world.");
        assert_eq!(chunks[0].chunk_size, 2);
        assert_eq!(chunks[0].chunk_index, 0);
    }

    #[test]
    fn batch_mode_accumulates() {
        let text = "First sentence. Second sentence. Third sentence.";
        let chunks = chunk_by_paragraph(text, 100, true, &WordCounter);
        // Should accumulate all into one chunk
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_size, 6);
    }

    #[test]
    fn batch_mode_overflow() {
        let text = "One two. Three four. Five six.";
        // Max 3 words: first sentence fits (2), second would overflow (2+2=4>3)
        let chunks = chunk_by_paragraph(text, 3, true, &WordCounter);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
    }

    #[test]
    fn non_batch_mode_yields_at_paragraph() {
        let text = "First paragraph.\nSecond paragraph.";
        let chunks = chunk_by_paragraph(text, 100, false, &WordCounter);
        // Should yield at each paragraph boundary
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn sequential_chunk_indices() {
        let text = "A. B. C. D. E.";
        let chunks = chunk_by_paragraph(text, 2, true, &WordCounter);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn deterministic_ids() {
        let text = "Hello world. Foo bar.";
        let chunks1 = chunk_by_paragraph(text, 100, true, &WordCounter);
        let chunks2 = chunk_by_paragraph(text, 100, true, &WordCounter);
        assert_eq!(chunks1[0].chunk_id, chunks2[0].chunk_id);
    }

    #[test]
    fn ground_truth_whole_text() {
        use crate::cut_type::CutType;

        let input = "The quick brown fox jumps over the lazy dog. It was a sunny day.\n\
                     The rain in Spain falls mainly on the plain. A stitch in time saves nine. An apple a day keeps the doctor away.\n\
                     To be or not to be that is the question. All that glitters is not gold. Actions speak louder than words. The pen is mightier than the sword. Knowledge is power above all else.";
        let counter = WordCounter;
        let chunks = chunk_by_paragraph(input, 12, true, &counter);

        // With max_chunk_size=12 and batch_paragraphs=true, the text is split
        // into multiple chunks by overflow. Each chunk respects the 12-word limit.
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );

        // All chunk sizes should be within the limit
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.chunk_size <= 12,
                "chunk {i} has size {} > 12",
                chunk.chunk_size
            );
        }

        // Last chunk — text ends with sentence-ending punctuation "."
        let last = chunks.last().unwrap();
        assert_eq!(last.cut_type, CutType::SentenceEnd);

        // Verify indices are sequential
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i, "chunk_index mismatch at {i}");
        }
    }

    #[test]
    fn ground_truth_cut_text() {
        use crate::cut_type::CutType;

        let input = "The quick brown fox jumps over the lazy dog. It was a sunny day.\n\
                     The rain in Spain falls mainly on the plain. A stitch in time saves nine. An apple a day keeps the doctor away.\n\
                     To be or not to be that is the question. All that glitters is not gold. Actions speak louder than words. The pen is mightier than the sword. Knowledge is power above all else";
        let counter = WordCounter;
        let chunks = chunk_by_paragraph(input, 12, true, &counter);

        assert!(chunks.len() >= 2, "expected at least 2 chunks");

        // Last chunk should be SentenceCut (no trailing punctuation)
        let last = chunks.last().unwrap();
        assert_eq!(
            last.cut_type,
            CutType::SentenceCut,
            "last chunk should be SentenceCut when text doesn't end with punctuation"
        );
    }
}
