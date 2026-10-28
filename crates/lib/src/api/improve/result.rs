//! Result types of one `improve()` run — Python's
//! `cognee/modules/improve/result.py`.
//!
//! [`StageResult`] is one entry per stage; [`ImproveResult`] holds one per
//! registry stage, in order, and is what every surface hands back. Both
//! serialise to Python's wire shape: snake_case keys, `"run": null` on each
//! stage, the deprecated `"memify_run": {}` and the computed `"status"`.

use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use uuid::Uuid;

/// Skip reason of a session-fed stage run without session ids.
pub const REASON_NO_SESSION_IDS: &str = "no_session_ids";
/// Skip reason of a stage named in the improve configuration's disabled list.
pub const REASON_DISABLED_BY_CONFIG: &str = "disabled_by_config";
/// Skip reason of every stage after a fatal stage that errored.
pub const REASON_ABORTED_BY_FATAL_STAGE: &str = "aborted_by_fatal_stage";
/// Skip reason of every stage after one that failed on an exhausted LLM
/// budget: each would fail the same way.
pub const REASON_BUDGET_EXHAUSTED: &str = "budget_exhausted";
pub use cognee_cognify::REASON_BACKEND_UNSUPPORTED;

/// Status of one improve stage — Python's `StageStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Completed,
    AlreadyCompleted,
    Skipped,
    Errored,
}

impl StageStatus {
    /// The wire name Python uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::AlreadyCompleted => "already_completed",
            Self::Skipped => "skipped",
            Self::Errored => "errored",
        }
    }
}

/// What one improve stage did in one run — Python's `StageResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: String,
    pub status: StageStatus,
    /// Why the stage was skipped (set whenever `status` is `Skipped`), or why
    /// it had nothing to do (`AlreadyCompleted`).
    pub reason: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub counts: BTreeMap<String, i64>,
    /// Wall time of the stage's run; `0` for a stage a gate skipped.
    #[serde(default)]
    pub duration_ms: u64,
    /// Python's `PipelineRunInfo` of a pipeline-backed stage. No Rust stage
    /// reports one, so it is always `None` (`"run": null` on the wire).
    #[serde(default)]
    pub run: Option<serde_json::Value>,
}

impl StageResult {
    pub fn skipped(stage: &str, reason: &str) -> Self {
        Self::new(stage, StageStatus::Skipped, Some(reason.to_string()), None)
    }

    pub fn already_completed(stage: &str, reason: &str) -> Self {
        Self::new(
            stage,
            StageStatus::AlreadyCompleted,
            Some(reason.to_string()),
            None,
        )
    }

    pub fn errored(stage: &str, error: impl Into<String>) -> Self {
        Self::new(stage, StageStatus::Errored, None, Some(error.into()))
    }

    pub fn completed(stage: &str) -> Self {
        Self::new(stage, StageStatus::Completed, None, None)
    }

    /// Set the stage counts, Python's `**counts`.
    pub fn with_counts<const N: usize>(mut self, counts: [(&str, i64); N]) -> Self {
        self.counts = counts
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        self
    }

    fn new(
        stage: &str,
        status: StageStatus,
        reason: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            stage: stage.to_string(),
            status,
            reason,
            error,
            counts: BTreeMap::new(),
            duration_ms: 0,
            run: None,
        }
    }
}

/// Summary status of one run — Python's `ImproveStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImproveStatus {
    Completed,
    Errored,
    Skipped,
    Running,
}

impl ImproveStatus {
    /// The wire name Python uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Errored => "errored",
            Self::Skipped => "skipped",
            Self::Running => "running",
        }
    }
}

/// One entry per stage, in registry order, for one `improve()` run — Python's
/// `ImproveResult` (`cognee/modules/improve/result.py:213-279`), which is also
/// the REST response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ImproveResult {
    /// The dataset the run worked on; `None` when it did not exist and no
    /// stage created it.
    pub dataset_id: Option<Uuid>,
    pub dataset_name: String,
    pub session_ids: Vec<String>,
    pub stages: Vec<StageResult>,
    /// The run was dispatched in the background by its host.
    pub background: bool,
    /// `false` only while a background run is still going.
    pub finished: bool,
    /// Set when a fatal stage aborted the run.
    pub error: Option<String>,
}

impl ImproveResult {
    /// A finished run with no stages recorded yet.
    pub fn new(dataset_name: impl Into<String>, session_ids: Vec<String>) -> Self {
        Self {
            dataset_id: None,
            dataset_name: dataset_name.into(),
            session_ids,
            stages: Vec::new(),
            background: false,
            finished: true,
            error: None,
        }
    }

    /// The result a host hands back for a run it dispatched in the
    /// background: `running`, with no stages yet.
    pub fn running_in_background(
        dataset_id: Option<Uuid>,
        dataset_name: impl Into<String>,
        session_ids: Vec<String>,
    ) -> Self {
        Self {
            dataset_id,
            background: true,
            finished: false,
            ..Self::new(dataset_name, session_ids)
        }
    }

    /// `running` while not finished; then `errored` if there is an error or
    /// any stage errored; then `skipped` if every stage was skipped;
    /// otherwise `completed`.
    pub fn status(&self) -> ImproveStatus {
        if !self.finished {
            return ImproveStatus::Running;
        }
        if self.error.is_some()
            || self
                .stages
                .iter()
                .any(|stage| stage.status == StageStatus::Errored)
        {
            return ImproveStatus::Errored;
        }
        if !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|stage| stage.status == StageStatus::Skipped)
        {
            return ImproveStatus::Skipped;
        }
        ImproveStatus::Completed
    }

    /// The result of the stage called `name`, if it was recorded.
    pub fn stage(&self, name: &str) -> Option<&StageResult> {
        self.stages.iter().find(|stage| stage.stage == name)
    }

    /// `name=status` pairs, comma-joined (Python's `stage_summary`).
    pub fn stage_summary(&self) -> String {
        self.stages
            .iter()
            .map(|stage| format!("{}={}", stage.stage, stage.status.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl Serialize for ImproveResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("ImproveResult", 9)?;
        out.serialize_field("dataset_id", &self.dataset_id)?;
        out.serialize_field("dataset_name", &self.dataset_name)?;
        out.serialize_field("session_ids", &self.session_ids)?;
        out.serialize_field("stages", &self.stages)?;
        out.serialize_field("background", &self.background)?;
        out.serialize_field("finished", &self.finished)?;
        out.serialize_field("error", &self.error)?;
        // Python's deprecated `memify_run` alias (SDK-1115): `{}` unless the
        // removed custom-tasks stage ran, which Rust never runs.
        out.serialize_field("memify_run", &serde_json::Map::new())?;
        out.serialize_field("status", &self.status())?;
        out.end()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use serde_json::json;

    fn result_with(statuses: &[StageStatus]) -> ImproveResult {
        let mut result = ImproveResult::new("docs", vec![]);
        result.stages = statuses
            .iter()
            .enumerate()
            .map(|(i, status)| StageResult {
                status: *status,
                reason: (*status == StageStatus::Skipped).then(|| "opt_in_disabled".to_string()),
                ..StageResult::completed(&format!("s{i}"))
            })
            .collect();
        result
    }

    #[test]
    fn stage_status_names_match_python() {
        let names: Vec<&str> = [
            StageStatus::Completed,
            StageStatus::AlreadyCompleted,
            StageStatus::Skipped,
            StageStatus::Errored,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(
            names,
            ["completed", "already_completed", "skipped", "errored"]
        );
        assert_eq!(
            serde_json::to_value(StageStatus::AlreadyCompleted).unwrap(),
            json!("already_completed")
        );
    }

    // Python result.py `ImproveResult.status`.
    #[test]
    fn status_is_derived_from_the_stages() {
        use StageStatus::*;
        assert_eq!(
            result_with(&[Completed, Skipped]).status(),
            ImproveStatus::Completed
        );
        assert_eq!(
            result_with(&[AlreadyCompleted, Skipped]).status(),
            ImproveStatus::Completed
        );
        assert_eq!(
            result_with(&[Skipped, Skipped]).status(),
            ImproveStatus::Skipped
        );
        assert_eq!(
            result_with(&[Completed, Errored, Skipped]).status(),
            ImproveStatus::Errored
        );
        // No stage at all is not "every stage skipped".
        assert_eq!(result_with(&[]).status(), ImproveStatus::Completed);

        let mut aborted = result_with(&[Completed]);
        aborted.error = Some("boom".into());
        assert_eq!(aborted.status(), ImproveStatus::Errored);

        let mut running = result_with(&[Errored]);
        running.finished = false;
        assert_eq!(running.status(), ImproveStatus::Running);
    }

    #[test]
    fn serialises_to_the_python_wire_shape() {
        let mut result = ImproveResult::new("docs", vec!["chat_1".into()]);
        result
            .stages
            .push(StageResult::completed("feedback_weights").with_counts([("sessions", 1)]));
        result
            .stages
            .push(StageResult::skipped("review_conflicts", "opt_in_disabled"));
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(
            wire,
            json!({
                "dataset_id": null,
                "dataset_name": "docs",
                "session_ids": ["chat_1"],
                "stages": [
                    {"stage": "feedback_weights", "status": "completed", "reason": null,
                     "error": null, "counts": {"sessions": 1}, "duration_ms": 0, "run": null},
                    {"stage": "review_conflicts", "status": "skipped",
                     "reason": "opt_in_disabled", "error": null, "counts": {},
                     "duration_ms": 0, "run": null},
                ],
                "background": false,
                "finished": true,
                "error": null,
                "memify_run": {},
                "status": "completed",
            })
        );

        // Round-trips; the computed status is ignored on the way in.
        let back: ImproveResult = serde_json::from_value(wire).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn a_background_run_reports_running_with_no_stages() {
        let result = ImproveResult::running_in_background(None, "docs", vec!["s".into()]);
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["status"], "running");
        assert_eq!(wire["finished"], false);
        assert_eq!(wire["background"], true);
        assert_eq!(wire["stages"], json!([]));
    }

    #[test]
    fn stage_summary_and_lookup() {
        let result = result_with(&[StageStatus::Completed, StageStatus::Skipped]);
        assert_eq!(result.stage_summary(), "s0=completed,s1=skipped");
        assert_eq!(
            result.stage("s1").map(|s| s.status),
            Some(StageStatus::Skipped)
        );
        assert!(result.stage("missing").is_none());
    }
}
