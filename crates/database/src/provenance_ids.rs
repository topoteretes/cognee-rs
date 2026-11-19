//! Deterministic ids of the relational provenance ledger rows (`node` /
//! `edge` tables).
//!
//! Every writer of ownership rows derives the row id and slug with these, so
//! a delete or rollback finds the rows whichever pipeline wrote them
//! (cognify's tasks, the deterministic code graph).

use uuid::Uuid;

/// Python's `str(tenant_id)`: `"None"` for no tenant.
fn tenant_text(tenant_id: Option<Uuid>) -> String {
    tenant_id.map_or_else(|| "None".to_string(), |tenant| tenant.to_string())
}

/// Deterministic node row id, Python's `upsert_nodes`:
/// `uuid5(NAMESPACE_OID, str(tenant_id) + str(user_id) + str(dataset_id) + str(data_id) + str(node_id))`.
///
/// When `tenant_id` is `None`, Python's `str(None)` produces `"None"`.
pub fn provenance_node_id(
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    data_id: Uuid,
    node_id: Uuid,
) -> Uuid {
    let tenant = tenant_text(tenant_id);
    let raw = format!("{tenant}{user_id}{dataset_id}{data_id}{node_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
}

/// Deterministic edge row id:
/// `uuid5(NAMESPACE_OID, str(tenant_id) + str(user_id) + str(dataset_id) + str(data_id) + str(source_id) + str(edge_text) + str(target_id))`.
///
/// A deliberate divergence from Python's `upsert_edges`, which leaves
/// `data_id` out: folding it in lets one edge keep one row per data item that
/// produced it, so it is removed only with its last owner. The rationale is
/// on cognify's `provenance_edge_id`.
pub fn provenance_edge_id(
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    data_id: Uuid,
    source_id: Uuid,
    edge_text: &str,
    target_id: Uuid,
) -> Uuid {
    let tenant = tenant_text(tenant_id);
    let raw = format!("{tenant}{user_id}{dataset_id}{data_id}{source_id}{edge_text}{target_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
}

/// Deterministic edge slug, the value edge ownership is compared on:
/// `uuid5(NAMESPACE_OID, normalize(str(source_id) + relationship_name + str(target_id)))`,
/// lower-cased, spaces to `_`, apostrophes dropped (matches `Triplet::new`).
pub fn triplet_slug(source_id: Uuid, relationship_name: &str, target_id: Uuid) -> Uuid {
    let raw = format!("{source_id}{relationship_name}{target_id}");
    let normalized = raw.to_lowercase().replace(' ', "_").replace('\'', "");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, normalized.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tenant_reads_as_python_none() {
        let (user, dataset, data, node) = (
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
        );
        let raw = format!("None{user}{dataset}{data}{node}");
        assert_eq!(
            provenance_node_id(None, user, dataset, data, node),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
        );
        assert_ne!(
            provenance_node_id(Some(Uuid::from_u128(9)), user, dataset, data, node),
            provenance_node_id(None, user, dataset, data, node)
        );
    }

    #[test]
    fn edge_rows_differ_per_data_item_and_share_a_slug() {
        let (user, dataset) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let (source, target) = (Uuid::from_u128(5), Uuid::from_u128(6));
        let first = provenance_edge_id(
            None,
            user,
            dataset,
            Uuid::from_u128(3),
            source,
            "calls",
            target,
        );
        let second = provenance_edge_id(
            None,
            user,
            dataset,
            Uuid::from_u128(4),
            source,
            "calls",
            target,
        );
        assert_ne!(first, second);
        assert_eq!(
            triplet_slug(source, "Is A", target),
            triplet_slug(source, "is_a", target)
        );
    }
}
