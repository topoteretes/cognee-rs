use cognee_utils::NAMESPACE_OID;
use uuid::Uuid;

/// Generate a deterministic dataset ID matching Python's formula:
///   `uuid5(NAMESPACE_OID, f"{dataset_name}{str(user_id)}{str(tenant_id)}")`
///
/// When `tenant_id` is `None`, the literal string `"None"` is appended,
/// matching Python's `str(None)` behavior.
pub fn generate_dataset_id(dataset_name: &str, user_id: Uuid, tenant_id: Option<Uuid>) -> Uuid {
    let input = match tenant_id {
        Some(tid) => format!("{dataset_name}{user_id}{tid}"),
        None => format!("{dataset_name}{user_id}None"),
    };
    Uuid::new_v5(&NAMESPACE_OID, input.as_bytes())
}

/// A deterministic data id for `identifier`, scoped to its owner, tenant and
/// (when given) dataset. Python's `get_unique_data_id` (modern derivation):
///   `uuid5(NAMESPACE_OID, f"{dataset_id}:{identifier}{user_id}{tenant_id}")`,
/// without the `{dataset_id}:` prefix when there is no dataset, and with
/// `"None"` for a missing tenant (Python's `str(None)`).
///
/// Python also probes for a row under the pre-tenant derivation
/// (`uuid5(identifier + user_id)`) and keeps that id when one exists; no row
/// written by this SDK predates tenant-aware ids, so the probe is not done.
pub fn generate_unique_data_id(
    identifier: &str,
    user_id: Uuid,
    tenant_id: Option<Uuid>,
    dataset_id: Option<Uuid>,
) -> Uuid {
    let identifier = match dataset_id {
        Some(dataset_id) => format!("{dataset_id}:{identifier}"),
        None => identifier.to_string(),
    };
    let tenant = tenant_id.map_or_else(|| "None".to_string(), |tenant| tenant.to_string());
    Uuid::new_v5(
        &NAMESPACE_OID,
        format!("{identifier}{user_id}{tenant}").as_bytes(),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_dataset_id_deterministic() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        let id1 = generate_dataset_id("my_dataset", user_id, None);
        let id2 = generate_dataset_id("my_dataset", user_id, None);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_dataset_id_different_names() {
        let user_id = Uuid::new_v4();
        let id1 = generate_dataset_id("dataset_a", user_id, None);
        let id2 = generate_dataset_id("dataset_b", user_id, None);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_generate_dataset_id_with_tenant() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let tenant_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

        let id1 = generate_dataset_id("ds", user_id, None);
        let id2 = generate_dataset_id("ds", user_id, Some(tenant_id));
        assert_ne!(id1, id2);
    }

    /// Python: `get_unique_data_id("code_repo:/r", user, dataset_id)` for a
    /// user without a tenant (value computed with Python's formula).
    #[test]
    fn unique_data_id_matches_pythons_formula() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let tenant_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let expected = Uuid::new_v5(
            &NAMESPACE_OID,
            format!("{dataset_id}:code_repo:/r{user_id}None").as_bytes(),
        );
        assert_eq!(
            generate_unique_data_id("code_repo:/r", user_id, None, Some(dataset_id)),
            expected
        );
        assert_eq!(
            generate_unique_data_id("code_repo:/r", user_id, Some(tenant_id), None),
            Uuid::new_v5(
                &NAMESPACE_OID,
                format!("code_repo:/r{user_id}{tenant_id}").as_bytes()
            )
        );
        // Python: `uuid5(NAMESPACE_OID, f"{dataset_id}:code_repo:/r{user_id}None")`.
        assert_eq!(expected.to_string(), "7637e9a3-20ba-53d4-920e-cacd8b8d1fbf");
    }

    /// Same test for dataset IDs.
    #[test]
    fn none_tenant_appends_literal_none_string_dataset() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        let id = generate_dataset_id("ds", user_id, None);

        let expected_input = format!("ds{user_id}None");
        let expected = Uuid::new_v5(&NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(id, expected);
    }
}
