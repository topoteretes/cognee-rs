//! Collapse repeated keys before a batched upsert.
//!
//! Every adapter writes nodes and edges in batches of one statement each, and
//! no backend copes with a key repeated inside one batch: Postgres rejects the
//! whole `INSERT … ON CONFLICT DO UPDATE` (`21000: … cannot affect row a second
//! time`), and Ladybug's `UNWIND … MERGE` aborts on a repeated edge key and, for
//! a repeated node id, writes the repeated row onto an unrelated node. So each
//! writer first reduces its input to one row per key, choosing the row that the
//! same upserts issued one at a time would leave behind.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;

/// For each distinct key, in first-seen order: the indices of its first and
/// last occurrence in `keys`.
///
/// One upsert per item, in order, leaves the **last** occurrence's values —
/// except for columns written only on insert (`created_at`), which keep the
/// **first** occurrence's. Callers take each column from the matching index.
/// The keys are only compared, never stored past the call, so a caller can
/// pass borrowed keys and serialize nothing but the survivors afterwards.
pub(crate) fn first_and_last_occurrence<K, I>(keys: I) -> Vec<(usize, usize)>
where
    K: Hash + Eq,
    I: IntoIterator<Item = K>,
{
    let keys = keys.into_iter();
    let mut position: HashMap<K, usize> = HashMap::with_capacity(keys.size_hint().0);
    let mut survivors: Vec<(usize, usize)> = Vec::with_capacity(keys.size_hint().0);
    for (index, key) in keys.enumerate() {
        match position.entry(key) {
            Entry::Occupied(slot) => survivors[*slot.get()].1 = index,
            Entry::Vacant(slot) => {
                slot.insert(survivors.len());
                survivors.push((index, index));
            }
        }
    }
    survivors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_first_seen_order_with_first_and_last_index() {
        let keys = ["a", "b", "a", "c", "b", "a"];
        assert_eq!(
            first_and_last_occurrence(keys),
            vec![(0, 5), (1, 4), (3, 3)]
        );
    }

    #[test]
    fn distinct_keys_pass_through() {
        assert_eq!(
            first_and_last_occurrence([1, 2, 3]),
            vec![(0, 0), (1, 1), (2, 2)]
        );
        assert!(first_and_last_occurrence(Vec::<u8>::new()).is_empty());
    }

    #[test]
    fn tuple_keys_compare_whole() {
        let keys = [("s", "t", "r"), ("s", "t", "x"), ("s", "t", "r")];
        assert_eq!(first_and_last_occurrence(keys), vec![(0, 2), (1, 1)]);
    }
}
