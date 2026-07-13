//! ID generation utilities.
//!
//! Provides deterministic UUID v5 generation for entity and edge identification
//! using NAMESPACE_OID.
//!
//! UUID v5 produces deterministic UUIDs based on a namespace and input data,
//! effectively functioning as content-addressed identifiers in UUID format.

use uuid::{Uuid, uuid};

/// Standard OID namespace UUID from RFC 4122.
/// Used for deterministic UUID v5 generation across the entire codebase.
///
/// Byte-identical to [`uuid::Uuid::NAMESPACE_OID`]; re-exported here for
/// ergonomic use so callers don't need a separate `uuid` dep just for the
/// constant.
pub const NAMESPACE_OID: Uuid = uuid!("6ba7b812-9dad-11d1-80b4-00c04fd430c8");

/// Generate a deterministic UUID from a node ID string.
///
/// **Normalization rules:**
/// - Convert to lowercase
/// - Replace spaces with underscores
/// - Remove apostrophes
///
/// This ensures that "Alice", "alice", and "Alice Smith" produce consistent,
/// deterministic UUIDs even with minor variations.
///
/// # Arguments
/// * `node_id` - Node identifier string (e.g., "Alice", "TechCorp", "Person")
///
/// # Returns
/// Deterministic UUID v5 based on normalized input
///
/// # Examples
/// ```
/// use cognee_utils::id_generation::generate_node_id;
///
/// let id1 = generate_node_id("Alice");
/// let id2 = generate_node_id("alice");
/// assert_eq!(id1, id2); // Same UUID despite case difference
///
/// let id3 = generate_node_id("Alice Smith");
/// // Produces UUID from "alice_smith"
/// ```
pub fn generate_node_id(node_id: &str) -> Uuid {
    let normalized = normalize_identifier(node_id);
    Uuid::new_v5(&NAMESPACE_OID, normalized.as_bytes())
}

/// Generate a deterministic, class-namespaced UUID for a `DataPoint` subclass.
///
/// Mirrors Python cognee's `DataPoint.id_for` — the single source of truth for
/// identity-bearing node ids:
///
/// ```text
/// uuid5(NAMESPACE_OID, f"{class_name}:{normalized_values.join('|')}")
/// ```
///
/// The class name is baked into the hash input so two different node kinds can
/// never collide on the same identity string (e.g. `Entity("institution")` vs
/// `EntityType("institution")` — the pre-namespacing collision fixed in
/// topoteretes/cognee#2510/#2515). Each value is normalized with
/// [`normalize_identifier`] (lowercase, spaces → `_`, apostrophes stripped),
/// byte-for-byte matching Python's `_normalize_identity_value`.
///
/// # Arguments
/// * `class_name` - The `DataPoint` subclass name (e.g. `"Entity"`, `"EntityType"`, `"EdgeType"`)
/// * `values` - The identity values (usually a single element; joined with `|`)
///
/// # Examples
/// ```
/// use cognee_utils::id_generation::data_point_id_for;
/// use uuid::Uuid;
///
/// assert_eq!(
///     data_point_id_for("Entity", &["Alice"]),
///     Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"Entity:alice"),
/// );
/// ```
pub fn data_point_id_for(class_name: &str, values: &[&str]) -> Uuid {
    let joined = values
        .iter()
        .map(|v| normalize_identifier(v))
        .collect::<Vec<_>>()
        .join("|");
    Uuid::new_v5(&NAMESPACE_OID, format!("{class_name}:{joined}").as_bytes())
}

/// Generate a normalized edge name string.
///
/// Unlike `generate_node_id()`, this returns a normalized string rather than a UUID,
/// as edge names are used as relationship labels in the graph database.
///
/// **Normalization rules:**
/// - Convert to lowercase
/// - Replace spaces with underscores
/// - Remove apostrophes
///
/// # Arguments
/// * `name` - Edge name/relationship type (e.g., "works at", "Works At", "located_in")
///
/// # Returns
/// Normalized edge name string
///
/// # Examples
/// ```
/// use cognee_utils::id_generation::generate_edge_name;
///
/// assert_eq!(generate_edge_name("works at"), "works_at");
/// assert_eq!(generate_edge_name("Works At"), "works_at");
/// assert_eq!(generate_edge_name("person's role"), "persons_role");
/// ```
pub fn generate_edge_name(name: &str) -> String {
    normalize_identifier(name)
}

/// Generate a normalized node name string.
///
/// Note: Unlike node IDs and edge names, this does NOT replace spaces with underscores.
/// This is used for display/search purposes while preserving readability.
///
/// **Normalization rules:**
/// - Convert to lowercase
/// - Remove apostrophes
/// - Keep spaces unchanged
///
/// # Arguments
/// * `name` - Node name (e.g., "Alice Smith", "TechCorp")
///
/// # Returns
/// Normalized node name string
///
/// # Examples
/// ```
/// use cognee_utils::id_generation::generate_node_name;
///
/// assert_eq!(generate_node_name("Alice Smith"), "alice smith");
/// assert_eq!(generate_node_name("O'Reilly"), "oreilly");
/// ```
pub fn generate_node_name(name: &str) -> String {
    name.to_lowercase().replace('\'', "")
}

/// Normalization helper for IDs and edge names.
///
/// Applies full normalization: lowercase, spaces → underscores, remove apostrophes.
/// Byte-for-byte matches Python cognee's `DataPoint._normalize_identity_value`.
pub fn normalize_identifier(input: &str) -> String {
    input.to_lowercase().replace(' ', "_").replace('\'', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_oid_constant() {
        // Verify it matches the standard OID namespace
        assert_eq!(
            NAMESPACE_OID.to_string(),
            "6ba7b812-9dad-11d1-80b4-00c04fd430c8"
        );
    }

    #[test]
    fn test_generate_node_id_deterministic() {
        let id1 = generate_node_id("Alice");
        let id2 = generate_node_id("Alice");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_node_id_case_insensitive() {
        let id1 = generate_node_id("Alice");
        let id2 = generate_node_id("alice");
        let id3 = generate_node_id("ALICE");
        assert_eq!(id1, id2);
        assert_eq!(id2, id3);
    }

    #[test]
    fn test_generate_node_id_normalizes_spaces() {
        let id1 = generate_node_id("Alice Smith");
        let id2 = generate_node_id("alice_smith");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_node_id_removes_apostrophes() {
        let id1 = generate_node_id("O'Reilly");
        let id2 = generate_node_id("OReilly");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_edge_name() {
        assert_eq!(generate_edge_name("works at"), "works_at");
        assert_eq!(generate_edge_name("Works At"), "works_at");
        assert_eq!(generate_edge_name("WORKS_AT"), "works_at");
    }

    #[test]
    fn test_generate_edge_name_removes_apostrophes() {
        assert_eq!(generate_edge_name("person's role"), "persons_role");
    }

    #[test]
    fn test_generate_node_name_preserves_spaces() {
        assert_eq!(generate_node_name("Alice Smith"), "alice smith");
        assert_eq!(generate_node_name("Tech Corp"), "tech corp");
    }

    #[test]
    fn test_generate_node_name_removes_apostrophes() {
        assert_eq!(generate_node_name("O'Reilly"), "oreilly");
    }

    #[test]
    fn test_normalize_identifier() {
        assert_eq!(normalize_identifier("Hello World"), "hello_world");
        assert_eq!(normalize_identifier("It's Great"), "its_great");
        assert_eq!(normalize_identifier("UPPER_CASE"), "upper_case");
    }

    #[test]
    fn test_data_point_id_for_matches_python() {
        // Python: uuid5(NAMESPACE_OID, f"{cls.__name__}:{normalized}")
        assert_eq!(
            data_point_id_for("Entity", &["Alice"]),
            Uuid::new_v5(&NAMESPACE_OID, b"Entity:alice"),
        );
        assert_eq!(
            data_point_id_for("EntityType", &["Organization"]),
            Uuid::new_v5(&NAMESPACE_OID, b"EntityType:organization"),
        );
        assert_eq!(
            data_point_id_for("EdgeType", &["works at"]),
            Uuid::new_v5(&NAMESPACE_OID, b"EdgeType:works_at"),
        );
    }

    #[test]
    fn test_data_point_id_for_class_namespaced() {
        // Same identity string, different class → different id (no collision).
        assert_ne!(
            data_point_id_for("Entity", &["institution"]),
            data_point_id_for("EntityType", &["institution"]),
        );
    }

    #[test]
    fn test_data_point_id_for_joins_multiple_values() {
        assert_eq!(
            data_point_id_for("Entity", &["Alice", "Bob"]),
            Uuid::new_v5(&NAMESPACE_OID, b"Entity:alice|bob"),
        );
    }
}
