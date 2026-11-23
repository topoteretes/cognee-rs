//! The units a cognify run builds its graph from.
//!
//! Python runs the cognify pipeline once per data item, and hands each task
//! `chunks_per_batch` chunks of that item at a time
//! (`run_tasks_base.py`, `task_config={"batch_size": chunks_per_batch}`).
//! Graph construction (`construct_data_points_and_edges`) and
//! `add_data_points` therefore see one data item, or one block of
//! `chunks_per_batch` chunks of it, with fresh maps. Rust extracts the whole
//! run at once and regroups the results into the same units.

use std::collections::HashMap;

use cognee_models::DocumentChunk;
use uuid::Uuid;

/// Group `items` into Python's units, in first-seen order.
///
/// Each item names its data item, its chunk and a value. The chunk's block is
/// its position among `chunks` of the same document, divided by
/// `chunks_per_batch` (0 counts as 1); a chunk missing from `chunks` falls in
/// block 0. Items keep their input order within a unit, and units are ordered
/// by their first item.
pub fn extraction_units<T>(
    chunks: &[DocumentChunk],
    chunks_per_batch: usize,
    items: impl IntoIterator<Item = (Uuid, Uuid, T)>,
) -> Vec<Vec<T>> {
    let chunks_per_batch = chunks_per_batch.max(1);
    let mut positions: HashMap<Uuid, usize> = HashMap::new();
    let chunk_block: HashMap<Uuid, usize> = chunks
        .iter()
        .map(|chunk| {
            let position = positions.entry(chunk.document_id).or_default();
            let block = *position / chunks_per_batch;
            *position += 1;
            (chunk.base.id, block)
        })
        .collect();

    let mut units: Vec<Vec<T>> = Vec::new();
    let mut unit_of: HashMap<(Uuid, usize), usize> = HashMap::new();
    for (data_id, chunk_id, item) in items {
        let key = (data_id, chunk_block.get(&chunk_id).copied().unwrap_or(0));
        let unit = *unit_of.entry(key).or_insert_with(|| {
            units.push(Vec::new());
            units.len() - 1
        });
        units[unit].push(item);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(document_id: Uuid, index: usize) -> DocumentChunk {
        DocumentChunk::new(
            Uuid::new_v4(),
            format!("chunk {index}"),
            1,
            index,
            "sentence_end".to_string(),
            document_id,
        )
    }

    #[test]
    fn chunks_group_by_document_and_block() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        // Interleaved on purpose: a block is counted per document.
        let chunks = vec![chunk(a, 0), chunk(b, 0), chunk(a, 1), chunk(a, 2)];
        let items = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| (c.document_id, c.base.id, i));

        assert_eq!(
            extraction_units(&chunks, 2, items.clone()),
            [vec![0, 2], vec![1], vec![3]]
        );
        assert_eq!(
            extraction_units(&chunks, 2000, items.clone()),
            [vec![0, 2, 3], vec![1]]
        );
        // 0 counts as 1: every chunk is its own unit.
        assert_eq!(extraction_units(&chunks, 0, items).len(), 4);
    }

    #[test]
    fn an_unknown_chunk_falls_in_its_data_items_first_block() {
        let a = Uuid::new_v4();
        let chunks = vec![chunk(a, 0)];
        let items = [
            (a, chunks[0].base.id, "known"),
            (a, Uuid::new_v4(), "unknown"),
        ];
        assert_eq!(
            extraction_units(&chunks, 1, items),
            [vec!["known", "unknown"]]
        );
    }
}
