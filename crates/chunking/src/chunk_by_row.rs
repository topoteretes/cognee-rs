//! Row-based chunking for CSV and DLT data.
//!
//! Ports Python's `cognee/tasks/chunks/chunk_by_row.py`.
//!
//! Input `data` is the full text content. Rows are delimited by `"\n\n"`.
//! Within each row, pairs are delimited by `", "`. Token counting uses
//! the provided [`TokenCounter`].

use uuid::Uuid;

use cognee_models::DocumentChunk;

use crate::cut_type::CutType;
use crate::text_chunker::NAMESPACE_OID;
use crate::token_counter::TokenCounter;

/// Chunk text by row boundaries, porting Python's `chunk_by_row`.
///
/// Produces [`DocumentChunk`] items with:
/// - `cut_type = "row_cut"` for mid-row splits (chunk exceeded max size)
/// - `cut_type = "row_end"` for end-of-row boundaries
/// - Sequential `chunk_index` starting at 0
/// - Deterministic UUID5 IDs based on `document_id` and `chunk_index`
pub fn chunk_by_row<C: TokenCounter>(
    document_id: Uuid,
    data: &str,
    max_chunk_size: usize,
    counter: &C,
) -> Vec<DocumentChunk> {
    let mut result = Vec::new();
    let mut chunk_index: usize = 0;

    // Split on "\n\n" to get rows (lines), matching Python's data.split("\n\n")
    let rows: Vec<&str> = data.split("\n\n").collect();

    for row in &rows {
        // Skip empty rows (e.g. from empty input or trailing "\n\n")
        if row.is_empty() {
            continue;
        }

        let mut current_chunk_list: Vec<&str> = Vec::new();
        let mut current_chunk_size: usize = 0;

        // Split each row on ", " to get pairs
        let pairs: Vec<&str> = row.split(", ").collect();

        for pair in &pairs {
            let pair_size = counter.count_tokens(pair);

            // If adding this pair would exceed the budget AND the chunk is non-empty,
            // emit the current chunk with cut_type="row_cut"
            if current_chunk_size + pair_size > max_chunk_size && !current_chunk_list.is_empty() {
                let chunk_text = current_chunk_list.join(", ");
                let chunk_id = Uuid::new_v5(
                    &NAMESPACE_OID,
                    format!("{}-{}", document_id, chunk_index).as_bytes(),
                );
                let word_count = counter.count_tokens(&chunk_text);
                result.push(DocumentChunk::new(
                    chunk_id,
                    chunk_text,
                    word_count,
                    chunk_index,
                    CutType::RowCut.to_string(),
                    document_id,
                ));
                chunk_index += 1;
                current_chunk_list = Vec::new();
                current_chunk_size = 0;
            }

            current_chunk_list.push(pair);
            current_chunk_size += pair_size;
        }

        // After processing all pairs in this row, emit accumulated chunk
        // with cut_type="row_end" (if non-empty)
        if !current_chunk_list.is_empty() {
            let chunk_text = current_chunk_list.join(", ");
            let chunk_id = Uuid::new_v5(
                &NAMESPACE_OID,
                format!("{}-{}", document_id, chunk_index).as_bytes(),
            );
            let word_count = counter.count_tokens(&chunk_text);
            result.push(DocumentChunk::new(
                chunk_id,
                chunk_text,
                word_count,
                chunk_index,
                CutType::RowEnd.to_string(),
                document_id,
            ));
            chunk_index += 1;
        }

        // Explicit reset after each row_end (fixes Python bug where state
        // leaks between rows). chunk_index is NOT reset -- it stays contiguous.
        // current_chunk_list and current_chunk_size are reset by the loop iteration.
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_counter::WordCounter;

    #[test]
    fn empty_input_returns_no_chunks() {
        let doc_id = Uuid::new_v4();
        let chunks = chunk_by_row(doc_id, "", 100, &WordCounter);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_pair_within_budget() {
        let doc_id = Uuid::new_v4();
        let chunks = chunk_by_row(doc_id, "key: value", 100, &WordCounter);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "key: value");
        assert_eq!(chunks[0].cut_type, "row_end");
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].document_id, doc_id);
    }

    #[test]
    fn multiple_pairs_exceeding_budget() {
        let doc_id = Uuid::new_v4();
        // Each pair is 2 words. Budget of 3 means: first pair fits (2), second pair
        // would make 4 > 3, so first chunk is emitted as row_cut, second pair becomes
        // the final row_end chunk.
        let data = "alpha bravo, charlie delta, echo foxtrot";
        let chunks = chunk_by_row(doc_id, data, 3, &WordCounter);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "alpha bravo");
        assert_eq!(chunks[0].cut_type, "row_cut");
        assert_eq!(chunks[0].chunk_index, 0);

        assert_eq!(chunks[1].text, "charlie delta");
        assert_eq!(chunks[1].cut_type, "row_cut");
        assert_eq!(chunks[1].chunk_index, 1);

        assert_eq!(chunks[2].text, "echo foxtrot");
        assert_eq!(chunks[2].cut_type, "row_end");
        assert_eq!(chunks[2].chunk_index, 2);
    }

    #[test]
    fn multi_row_input() {
        let doc_id = Uuid::new_v4();
        let data = "row1_a, row1_b\n\nrow2_a, row2_b";
        let chunks = chunk_by_row(doc_id, data, 100, &WordCounter);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "row1_a, row1_b");
        assert_eq!(chunks[0].cut_type, "row_end");
        assert_eq!(chunks[0].chunk_index, 0);

        assert_eq!(chunks[1].text, "row2_a, row2_b");
        assert_eq!(chunks[1].cut_type, "row_end");
        assert_eq!(chunks[1].chunk_index, 1);
    }

    #[test]
    fn chunk_index_is_contiguous_across_rows() {
        let doc_id = Uuid::new_v4();
        // Row 1 will produce 2 chunks (row_cut + row_end), row 2 produces 1 chunk (row_end)
        let data = "a b, c d, e f\n\ng h";
        let chunks = chunk_by_row(doc_id, data, 3, &WordCounter);

        // Row 1: "a b" (2 tokens), "c d" (2 tokens) -> 4 > 3, so "a b" emitted as row_cut
        // Then "c d" (2), "e f" (2) -> 4 > 3, so "c d" emitted as row_cut
        // Then "e f" emitted as row_end
        // Row 2: "g h" emitted as row_end
        assert_eq!(chunks.len(), 4);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i, "chunk_index should be contiguous");
        }
        assert_eq!(chunks[0].cut_type, "row_cut");
        assert_eq!(chunks[1].cut_type, "row_cut");
        assert_eq!(chunks[2].cut_type, "row_end");
        assert_eq!(chunks[3].cut_type, "row_end");
    }

    #[test]
    fn isomorphism_per_row() {
        let doc_id = Uuid::new_v4();
        let row1 = "col1: val1, col2: val2, col3: val3";
        let row2 = "col1: valA, col2: valB";
        let data = format!("{}\n\n{}", row1, row2);
        let chunks = chunk_by_row(doc_id, &data, 100, &WordCounter);

        // With large budget, each row is a single chunk
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, row1);
        assert_eq!(chunks[1].text, row2);
    }

    #[test]
    fn isomorphism_with_splits() {
        let doc_id = Uuid::new_v4();
        // Single row, budget forces splits
        let data = "a, b, c, d";
        let chunks = chunk_by_row(doc_id, data, 1, &WordCounter);

        // Each pair is 1 token. Budget is 1, so each pair is its own chunk.
        // "a" -> row_cut (because next pair would overflow), "b" -> row_cut,
        // "c" -> row_cut, "d" -> row_end
        assert_eq!(chunks.len(), 4);

        // Joining all chunk texts with ", " should reconstruct the original
        let reconstructed: String = chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(reconstructed, data);
    }
}
