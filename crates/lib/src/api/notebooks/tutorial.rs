//! Tutorial notebook seeder — thin re-export from `cognee-database`.
//!
//! The actual implementation (including the `include_dir!` bundling of cell
//! assets) lives in `cognee_database::ops::tutorial_seeder` so that both
//! `cognee-lib` AND `cognee-http-server` can invoke the seeder without
//! introducing a dependency on `cognee-lib` from the HTTP server.

pub use cognee_database::{
    TUTORIAL_BASICS_ID, TUTORIAL_PYTHON_DEV_ID, seed_tutorials_if_first_call,
};

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn tutorial_ids_are_deterministic() {
        let basics = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            "Cognee Basics - tutorial 🧠".as_bytes(),
        );
        assert_eq!(
            basics, TUTORIAL_BASICS_ID,
            "TUTORIAL_BASICS_ID must match uuid5(NAMESPACE_OID, 'Cognee Basics - tutorial 🧠')"
        );

        let python_dev = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            "Python Development with Cognee - tutorial 🧠".as_bytes(),
        );
        assert_eq!(
            python_dev, TUTORIAL_PYTHON_DEV_ID,
            "TUTORIAL_PYTHON_DEV_ID must match uuid5(NAMESPACE_OID, 'Python Development with Cognee - tutorial 🧠')"
        );
    }
}
