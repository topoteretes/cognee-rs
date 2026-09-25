//! Deterministic pipeline & pipeline-run IDs (Python parity).
//!
//! Shared between the HTTP server's `dispatch_pipeline` and library-level
//! callers such as the reset helpers in `cognee`. Promoted out of
//! `crates/http-server/src/pipelines/dispatch.rs` (action item 08-05 §4.0)
//! so `cognee` can call them without depending on `cognee-http-server`.
//!
//! Both helpers produce byte-identical values to the Python utilities:
//!
//! - [Python `generate_pipeline_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py)
//! - [Python `generate_pipeline_run_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py)

use uuid::Uuid;

/// `pipeline_id = uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
///
/// `dataset_id` defaults to [`Uuid::nil`] when absent (ad-hoc paths).
///
/// Matches [Python's `generate_pipeline_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py).
pub fn pipeline_id(user_id: Uuid, dataset_id: Uuid, pipeline_name: &str) -> Uuid {
    let s = format!("{user_id}{pipeline_name}{dataset_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}

/// `pipeline_run_id = uuid5(OID, "{pipeline_id}_{dataset_id}")`
///
/// Matches [Python's `generate_pipeline_run_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py).
///
/// Note: this id is **not unique across separate runs** of the same pipeline —
/// Python intentionally reuses it so a re-cognify of the same dataset returns
/// the same `pipeline_run_id`. The `id` column in `pipeline_runs` is the true
/// PK; multiple rows can share the same `pipeline_run_id`.
pub fn pipeline_run_id(pipeline_id: Uuid, dataset_id: Uuid) -> Uuid {
    let s = format!("{pipeline_id}_{dataset_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
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
    fn pipeline_id_is_deterministic() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001")
            .expect("valid nil-adjacent UUID literal");
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002")
            .expect("valid nil-adjacent UUID literal");

        let a = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        let b = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        assert_eq!(a, b);
    }

    #[test]
    fn pipeline_id_differs_on_name() {
        let user_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let cognify = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        let memify = pipeline_id(user_id, dataset_id, "memify_pipeline");
        assert_ne!(cognify, memify);
    }

    #[test]
    fn pipeline_run_id_is_deterministic() {
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();
        assert_eq!(pipeline_run_id(pid, did), pipeline_run_id(pid, did));
    }

    #[test]
    fn pipeline_run_id_differs_on_dataset() {
        let pid = Uuid::new_v4();
        let did1 = Uuid::new_v4();
        let did2 = Uuid::new_v4();
        assert_ne!(pipeline_run_id(pid, did1), pipeline_run_id(pid, did2));
    }
}
