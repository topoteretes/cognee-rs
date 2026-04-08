use cognee_utils::NAMESPACE_OID;
use uuid::Uuid;

/// Generate a deterministic data ID matching Python's formula:
///   `uuid5(NAMESPACE_OID, f"{content_hash}{str(user_id)}{str(tenant_id)}")`
///
/// When `tenant_id` is `None`, Python's `str(None)` produces the literal
/// string `"None"`, so the formula becomes:
///   `uuid5(NAMESPACE_OID, f"{content_hash}{str(user_id)}None")`
///
/// Python's `str(uuid)` and Rust's `format!("{}", uuid)` both produce the
/// hyphenated lowercase form, so the inputs are identical.
pub fn generate_data_id(content_hash: &str, user_id: Uuid, tenant_id: Option<Uuid>) -> Uuid {
    let input = match tenant_id {
        Some(tid) => format!("{}{}{}", content_hash, user_id, tid),
        None => format!("{}{}None", content_hash, user_id),
    };
    Uuid::new_v5(&NAMESPACE_OID, input.as_bytes())
}

/// Generate a deterministic dataset ID matching Python's formula:
///   `uuid5(NAMESPACE_OID, f"{dataset_name}{str(user_id)}{str(tenant_id)}")`
///
/// When `tenant_id` is `None`, the literal string `"None"` is appended,
/// matching Python's `str(None)` behavior.
pub fn generate_dataset_id(dataset_name: &str, user_id: Uuid, tenant_id: Option<Uuid>) -> Uuid {
    let input = match tenant_id {
        Some(tid) => format!("{}{}{}", dataset_name, user_id, tid),
        None => format!("{}{}None", dataset_name, user_id),
    };
    Uuid::new_v5(&NAMESPACE_OID, input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_data_id_deterministic() {
        let hash = "5eb63bbbe01eeed093cb22bb8f5acdc3";
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        let id1 = generate_data_id(hash, user_id, None);
        let id2 = generate_data_id(hash, user_id, None);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_data_id_with_tenant() {
        let hash = "5eb63bbbe01eeed093cb22bb8f5acdc3";
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let tenant_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

        let id_no_tenant = generate_data_id(hash, user_id, None);
        let id_with_tenant = generate_data_id(hash, user_id, Some(tenant_id));
        // Different because tenant_id changes the input string
        assert_ne!(id_no_tenant, id_with_tenant);
    }

    #[test]
    fn test_generate_data_id_different_users_different_ids() {
        let hash = "5eb63bbbe01eeed093cb22bb8f5acdc3";
        let user1 = Uuid::new_v4();
        let user2 = Uuid::new_v4();

        let id1 = generate_data_id(hash, user1, None);
        let id2 = generate_data_id(hash, user2, None);
        assert_ne!(id1, id2);
    }

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

    /// Verify that None tenant appends the literal string "None" to match
    /// Python's `str(None)` behavior in the UUID5 input.
    #[test]
    fn none_tenant_appends_literal_none_string() {
        let hash = "abc";
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        let id = generate_data_id(hash, user_id, None);

        // Manually compute what Python produces:
        //   uuid5(NAMESPACE_OID, f"{hash}{user_id}None")
        let expected_input = format!("{}{}None", hash, user_id);
        let expected = Uuid::new_v5(&NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(
            id, expected,
            "None tenant must append literal 'None' to match Python"
        );
    }

    /// Same test for dataset IDs.
    #[test]
    fn none_tenant_appends_literal_none_string_dataset() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        let id = generate_dataset_id("ds", user_id, None);

        let expected_input = format!("ds{}None", user_id);
        let expected = Uuid::new_v5(&NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(id, expected);
    }
}
