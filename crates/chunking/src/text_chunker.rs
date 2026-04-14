//! Top-level text chunker producing `DocumentChunk` output.
//!
//! Uses `chunk_by_paragraph` internally, then further batches paragraph chunks
//! until the max chunk size would be exceeded. Produces deterministic UUIDs
//! based on document ID and chunk index.
//!
//! Port of Python `cognee.modules.chunking.TextChunker`.

use uuid::{Uuid, uuid};

use cognee_models::DocumentChunk;

use crate::chunk_by_paragraph::chunk_by_paragraph;
use crate::token_counter::TokenCounter;

/// NAMESPACE_OID from the uuid spec.
pub const NAMESPACE_OID: Uuid = uuid!("6ba7b812-9dad-11d1-80b4-00c04fd430c8");

/// Chunks text from a document into `DocumentChunk` items.
///
/// Algorithm:
/// 1. Run `chunk_by_paragraph(batch_paragraphs=true)` to get paragraph-level chunks.
/// 2. Accumulate paragraph chunks until adding the next would exceed `max_chunk_size`.
/// 3. On overflow, emit the accumulated text (joined with space) as a DocumentChunk.
/// 4. If a single paragraph exceeds `max_chunk_size` on its own (oversized), emit it as-is.
/// 5. Emit any remaining accumulated text at the end.
pub fn chunk_text<C: TokenCounter>(
    document_id: Uuid,
    text: &str,
    max_chunk_size: usize,
    counter: &C,
) -> Vec<DocumentChunk> {
    let paragraph_chunks = chunk_by_paragraph(text, max_chunk_size, true, counter);
    let mut result = Vec::new();
    let mut accumulated: Vec<&crate::chunk_by_paragraph::ParagraphChunk<'_>> = Vec::new();
    let mut accumulated_size: usize = 0;
    let mut chunk_index: usize = 0;

    for para in &paragraph_chunks {
        if accumulated_size + para.chunk_size <= max_chunk_size {
            accumulated.push(para);
            accumulated_size += para.chunk_size;
        } else if accumulated.is_empty() {
            result.push(DocumentChunk::new(
                para.chunk_id,
                para.text.to_owned(),
                para.chunk_size,
                chunk_index,
                para.cut_type.to_string(),
                document_id,
            ));
            chunk_index += 1;
        } else {
            let chunk_text: String = accumulated
                .iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
                .join(" ");
            let cut_type = accumulated.last().expect("accumulated is non-empty because the else branch is only entered when !accumulated.is_empty()").cut_type.to_string();
            result.push(DocumentChunk::new(
                Uuid::new_v5(
                    &NAMESPACE_OID,
                    format!("{}-{}", document_id, chunk_index).as_bytes(),
                ),
                chunk_text,
                accumulated_size,
                chunk_index,
                cut_type,
                document_id,
            ));
            chunk_index += 1;
            accumulated = vec![para];
            accumulated_size = para.chunk_size;
        }
    }

    // Emit remaining
    if !accumulated.is_empty() {
        let chunk_text: String = accumulated
            .iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join(" ");
        let cut_type = accumulated.last().expect("accumulated is non-empty because the if-guard !accumulated.is_empty() was checked above").cut_type.to_string();
        result.push(DocumentChunk::new(
            Uuid::new_v5(
                &NAMESPACE_OID,
                format!("{}-{}", document_id, chunk_index).as_bytes(),
            ),
            chunk_text,
            accumulated_size,
            chunk_index,
            cut_type,
            document_id,
        ));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_counter::WordCounter;

    #[test]
    fn empty_input() {
        let doc_id = Uuid::new_v4();
        let chunks = chunk_text(doc_id, "", 100, &WordCounter);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_short_paragraph() {
        let doc_id = Uuid::new_v4();
        let chunks = chunk_text(doc_id, "Hello world.", 100, &WordCounter);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello world.");
        assert_eq!(chunks[0].chunk_size, 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].document_id, doc_id);
    }

    #[test]
    fn multiple_small_paragraphs_batch() {
        let doc_id = Uuid::new_v4();
        let text = "First. Second. Third.";
        let chunks = chunk_text(doc_id, text, 100, &WordCounter);
        // All fit into one chunk
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn overflow_creates_multiple_chunks() {
        let doc_id = Uuid::new_v4();
        // Each sentence is 2 words, max 3 words per chunk
        let text = "One two. Three four. Five six.";
        let chunks = chunk_text(doc_id, text, 3, &WordCounter);
        assert!(chunks.len() >= 2);
        // Check sequential indices
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn deterministic_uuids() {
        let doc_id = Uuid::new_v4();
        let text = "Hello world. This is a test.";
        let chunks1 = chunk_text(doc_id, text, 100, &WordCounter);
        let chunks2 = chunk_text(doc_id, text, 100, &WordCounter);
        assert_eq!(chunks1[0].base.id, chunks2[0].base.id);
    }

    #[test]
    fn document_id_propagated() {
        let doc_id = Uuid::new_v4();
        let chunks = chunk_text(doc_id, "Hello.", 100, &WordCounter);
        assert_eq!(chunks[0].document_id, doc_id);
    }

    #[test]
    fn chunk_index_sequential() {
        let doc_id = Uuid::new_v4();
        let text = "A. B. C. D. E. F. G. H.";
        let chunks = chunk_text(doc_id, text, 2, &WordCounter);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn whitespace_only_input_emits_single_chunk() {
        let doc_id = Uuid::new_v4();
        let input = "   \n\t   \r\n   ";
        let chunks = chunk_text(doc_id, input, 512, &WordCounter);
        assert_eq!(chunks.len(), 1, "whitespace-only should emit 1 chunk");
        assert_eq!(chunks[0].text, input);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].document_id, doc_id);
    }

    #[test]
    fn oversized_paragraph_emitted() {
        let doc_id = Uuid::new_v4();
        let long_para: String = (0..100)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let input = format!("{}.\nShort text here.", long_para);
        let chunks = chunk_text(doc_id, &input, 50, &WordCounter);
        // The 100-word sentence is pre-split by chunk_by_sentence into 2 chunks
        // of 50 words, plus the short paragraph makes 3 total
        assert!(
            chunks.len() >= 3,
            "should have at least 3 chunks, got {}",
            chunks.len()
        );
        // Each of the first two chunks should have the maximum allowed size
        assert_eq!(
            chunks[0].chunk_size, 50,
            "first chunk should be at the size limit"
        );
        assert_eq!(
            chunks[1].chunk_size, 50,
            "second chunk should be at the size limit"
        );
        // Last chunk is the short paragraph
        let last = chunks.last().unwrap();
        assert!(
            last.chunk_size < 50,
            "last chunk should be smaller than the limit"
        );
    }

    #[test]
    fn alternating_large_small_paragraphs_dont_batch() {
        let doc_id = Uuid::new_v4();
        let large: String = (0..20)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let small = "Short text here.";
        let input = format!("{large}.\n{small}\n{large}.\n{small}");
        let chunks = chunk_text(doc_id, &input, 10, &WordCounter);
        // Each 20-word "large" paragraph is pre-split into 2 chunks of 10 by
        // chunk_by_sentence, resulting in 6 total chunks:
        //   large_part1(10), large_part2(10), small(3),
        //   large_part1(10), large_part2(10), small(3)
        assert_eq!(
            chunks.len(),
            6,
            "expected 6 chunks (2*large_split + small + 2*large_split + small)"
        );
        assert_eq!(chunks[0].chunk_size, 10, "chunk 0 should be at limit");
        assert_eq!(chunks[1].chunk_size, 10, "chunk 1 should be at limit");
        assert!(
            chunks[2].chunk_size <= 10,
            "chunk 2 (small) should fit within limit"
        );
        assert_eq!(chunks[3].chunk_size, 10, "chunk 3 should be at limit");
        assert_eq!(chunks[4].chunk_size, 10, "chunk 4 should be at limit");
        assert!(
            chunks[5].chunk_size <= 10,
            "chunk 5 (small) should fit within limit"
        );
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, i, "chunk_index sequential at {i}");
        }
    }
}
