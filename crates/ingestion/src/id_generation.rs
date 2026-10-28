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
