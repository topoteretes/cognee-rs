//! In-memory no-op [`PipelineRunRepository`] for embedded uses without a DB.
//!
//! Library convenience functions (`cognify`, `memify`, `AddPipeline::add`)
//! now take an `Arc<dyn PipelineRunRepository>` (gap 08-07). Embedded users
//! (no SQL database wired) construct `Arc::new(NoopPipelineRunRepository::new())`
//! to satisfy the parameter without paying for any writes.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::pipelines::repository::{
    PipelineRunRepository, PipelineRunRow, PipelineRunWithAttributionRow,
};
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};

/// `PipelineRunRepository` that ignores all writes and returns empty
/// results for reads.
///
/// Suitable for tests and embedded library users that don't have a SQL
/// database. All write methods return `Ok(...)`; all read methods return
/// empty results (`None`, `Vec::new()`, empty `HashMap`/`Map`).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopPipelineRunRepository;

impl NoopPipelineRunRepository {
    /// Construct a new no-op repository.
    pub const fn new() -> Self {
        Self
    }

    /// Convenience: return as `Arc<dyn PipelineRunRepository>`.
    pub fn arc() -> Arc<dyn PipelineRunRepository> {
        Arc::new(Self)
    }
}

#[async_trait]
impl PipelineRunRepository for NoopPipelineRunRepository {
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        _pipeline_id: Uuid,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
        _status: PipelineRunStatus,
        _run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DatabaseError> {
        // Mirror the original http-server `NoOpPipelineRunRepository`
        // behaviour: echo the caller's `pipeline_run_id` so logical run
        // identity (used as the slot key in `DefaultPipelineRunRegistry`)
        // round-trips even when persistence is disabled.
        Ok(pipeline_run_id)
    }

    async fn latest_status(
        &self,
        _dataset_ids: &[Uuid],
        _pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DatabaseError> {
        Ok(HashMap::new())
    }

    async fn list_recent(
        &self,
        _dataset_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DatabaseError> {
        Ok(Vec::new())
    }

    async fn list_recent_with_attribution(
        &self,
        _dataset_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DatabaseError> {
        Ok(Vec::new())
    }

    async fn reset_orphans(&self, _reason: &str) -> Result<u64, DatabaseError> {
        Ok(0)
    }

    async fn set_payload_field(
        &self,
        _run_id: Uuid,
        _key: &str,
        _value: serde_json::Value,
    ) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn get_payload(
        &self,
        _run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DatabaseError> {
        Ok(serde_json::Map::new())
    }

    async fn get_pipeline_run(
        &self,
        _pipeline_run_id: Uuid,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        Ok(None)
    }

    async fn get_pipeline_run_by_dataset(
        &self,
        _dataset_id: Uuid,
        _pipeline_name: &str,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        Ok(None)
    }

    async fn get_pipeline_runs_by_dataset(
        &self,
        _dataset_id: Uuid,
    ) -> Result<Vec<PipelineRun>, DatabaseError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_echo_run_id_reads_return_empty() {
        let repo = NoopPipelineRunRepository::new();
        let run_id = Uuid::new_v4();
        let echoed = repo
            .log_pipeline_run(
                run_id,
                Uuid::new_v4(),
                "test_pipeline",
                None,
                PipelineRunStatus::Initiated,
                None,
            )
            .await
            .expect("log_pipeline_run on noop never fails");
        assert_eq!(echoed, run_id);

        assert!(
            repo.latest_status(&[], "p")
                .await
                .expect("latest_status")
                .is_empty()
        );
        assert!(
            repo.list_recent(None, 10)
                .await
                .expect("list_recent")
                .is_empty()
        );
        assert!(
            repo.list_recent_with_attribution(None, 10)
                .await
                .expect("list_recent_with_attribution")
                .is_empty()
        );
        assert_eq!(repo.reset_orphans("test").await.expect("reset_orphans"), 0);
        assert!(
            repo.get_pipeline_run(Uuid::new_v4())
                .await
                .expect("get_pipeline_run")
                .is_none()
        );
    }
}
