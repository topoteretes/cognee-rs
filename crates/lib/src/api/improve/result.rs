//! Result types of one `improve()` run — Python's
//! `cognee/modules/improve/result.py`.
//!
//! [`StageResult`] is one entry per stage; [`ImproveResult`] holds one per
//! registry stage, in order, and is what every surface hands back. Both
//! serialise to Python's wire shape: snake_case keys, each stage's `"run"`
//! (the [`StageRunInfo`] of a pipeline-backed stage that completed, else
//! `null`), the deprecated `"memify_run": {}` and the computed `"status"`.

use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
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
    /// The memify run of a pipeline-backed stage that completed (Python's
    /// `StageResult.run`, set by `StageResult.from_pipeline_run`); `None`
    /// (`"run": null`) for every other stage and outcome. See
    /// [`StageRunInfo`] for which stages carry one. Read back only when it
    /// is a completed memify run; any other run a remote result carries
    /// (see [`StageRunInfo`]'s `Deserialize`) reads as `None`.
    #[serde(default, deserialize_with = "deserialize_stage_run")]
    pub run: Option<StageRunInfo>,
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

    /// Attach the stage's memify run (see [`StageRunInfo`]).
    pub fn with_run(mut self, run: Option<StageRunInfo>) -> Self {
        self.run = run;
        self
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

/// The memify run a pipeline-backed improve stage reports in its `run` —
/// Python's `PipelineRunCompleted`, as `StageResult.from_pipeline_run` keeps
/// it (`cognee/modules/improve/result.py:131-156`).
///
/// In Python `feedback_weights`, `persist_session_qa`, `persist_agent_traces`
/// and `global_context_index` each run their tasks as one `memify_pipeline`
/// run (`cognee/modules/memify/run_memify_tasks.py`), so their `run` is that
/// run's info; the other stages, and a stage that was skipped or
/// `already_completed` before its pipeline ran, carry `null`. A memify run
/// that fails raises out of the stage, which is then `errored` with
/// `run: null`, so only a completed run is ever reported. Rust runs the same
/// four stages without a pipeline and fills the same info once one of them
/// completes on an existing dataset.
///
/// Only ids are kept (SDK-507). The wire shape is fixed by them: memify runs
/// over one placeholder item (`data=[{}]`, not a `Data` row) without
/// incremental loading, so `data_ingestion_info` is always one
/// `{"run_info": <the same completed run>, "data_id": null}` entry and
/// `data_ids` is `[]` (`run_tasks_data_item_regular`,
/// `cognee/modules/pipelines/operations/run_tasks_data_item.py:391-459`).
/// The per-document entry `/cognify` renders (`data_name`,
/// `data_location`, `data_label`) belongs to incremental runs over `Data`
/// rows and never appears here.
///
/// `pipeline_run_id` is fresh per stage run, as Python's `uuid4()` one;
/// Rust records no `memify_pipeline` row under it.
///
/// Only a completed memify-shaped run round-trips: `Deserialize` refuses a
/// run whose `status` is not `PipelineRunCompleted` or whose `data_ids` is
/// not empty, since the ids alone could not rebuild it (a Python server's
/// `review_conflicts` or `custom_tasks` run, an errored run). On a
/// [`StageResult`] such a run reads as `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRunInfo {
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    pub dataset_name: String,
}

/// The `status` of every [`StageRunInfo`] on the wire.
const PIPELINE_RUN_COMPLETED: &str = "PipelineRunCompleted";

impl StageRunInfo {
    /// A completed run of one stage over `dataset_id`, under a fresh run id.
    pub fn completed(dataset_id: Uuid, dataset_name: impl Into<String>) -> Self {
        Self {
            pipeline_run_id: Uuid::new_v4(),
            dataset_id,
            dataset_name: dataset_name.into(),
        }
    }

    /// Python's serialized `PipelineRunInfo` without its per-item fields.
    fn run_info_json(&self, data_ingestion_info: serde_json::Value) -> serde_json::Value {
        json!({
            "status": PIPELINE_RUN_COMPLETED,
            "pipeline_run_id": self.pipeline_run_id,
            "dataset_id": self.dataset_id,
            "dataset_name": self.dataset_name,
            "payload": null,
            "data_ingestion_info": data_ingestion_info,
        })
    }
}

impl Serialize for StageRunInfo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The nested run info carries no per-item list, so Python leaves its
        // computed `data_ids` out (`exclude_if=None`).
        let entry = json!({
            "run_info": self.run_info_json(serde_json::Value::Null),
            "data_id": null,
        });
        let mut run = self.run_info_json(json!([entry]));
        if let Some(fields) = run.as_object_mut() {
            fields.insert("data_ids".to_string(), json!([]));
        }
        run.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StageRunInfo {
    /// Reads the ids back from a completed memify run (ours or a Python
    /// server's); its per-item list is fixed by them. Any other run is an
    /// error (see the type's docs).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            status: String,
            pipeline_run_id: Uuid,
            dataset_id: Uuid,
            dataset_name: String,
            #[serde(default)]
            data_ids: Option<Vec<serde_json::Value>>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.status != PIPELINE_RUN_COMPLETED {
            return Err(serde::de::Error::custom(format!(
                "not a completed memify run: status {}",
                wire.status
            )));
        }
        if wire.data_ids.is_some_and(|ids| !ids.is_empty()) {
            return Err(serde::de::Error::custom(
                "not a memify run: it lists data ids",
            ));
        }
        Ok(Self {
            pipeline_run_id: wire.pipeline_run_id,
            dataset_id: wire.dataset_id,
            dataset_name: wire.dataset_name,
        })
    }
}

/// `StageResult::run` from the wire: a completed memify run, else `None`.
fn deserialize_stage_run<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<StageRunInfo>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|run| serde_json::from_value(run).ok()))
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

    /// Why the run went wrong, if it did, as Python's `_improve_error_text`
    /// (`cognee/api/v1/remember/remember.py:73-91`): the fatal error, else the
    /// errored stages as `"<stage>: <error>"` joined by `"; "`.
    pub fn error_text(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(error.clone());
        }
        let errored: Vec<String> = self
            .stages
            .iter()
            .filter(|s| s.status == StageStatus::Errored)
            .map(|s| format!("{}: {}", s.stage, s.error.as_deref().unwrap_or("errored")))
            .collect();
        (!errored.is_empty()).then(|| errored.join("; "))
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
                reason: (*status == StageStatus::Skipped).then(|| "turned_off".to_string()),
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
            .push(StageResult::skipped("review_conflicts", "turned_off"));
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
                     "reason": "turned_off", "error": null, "counts": {},
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

    // Python `StageResult.from_pipeline_run` over a completed memify run
    // (`data=[{}]`, no incremental loading): one per-item entry with no
    // data id, and no `data_ids` on the nested run info.
    #[test]
    fn a_stage_run_serialises_as_pythons_completed_memify_run() {
        let run = StageRunInfo {
            pipeline_run_id: Uuid::from_u128(1),
            dataset_id: Uuid::from_u128(2),
            dataset_name: "docs".to_string(),
        };
        let stage = StageResult::completed("global_context_index").with_run(Some(run.clone()));
        let wire = serde_json::to_value(&stage).unwrap();
        let run_info = json!({
            "status": "PipelineRunCompleted",
            "pipeline_run_id": "00000000-0000-0000-0000-000000000001",
            "dataset_id": "00000000-0000-0000-0000-000000000002",
            "dataset_name": "docs",
            "payload": null,
            "data_ingestion_info": null,
        });
        let mut expected = run_info.clone();
        expected["data_ingestion_info"] = json!([{"run_info": run_info, "data_id": null}]);
        expected["data_ids"] = json!([]);
        assert_eq!(wire["run"], expected);
        // Python's key order (field order, then the computed `data_ids`).
        // `serde_json`'s `preserve_order` is on workspace-wide (root
        // `Cargo.toml`), so the order the run is built in is the wire order.
        assert_eq!(
            serde_json::to_string(&wire["run"]).unwrap(),
            concat!(
                r#"{"status":"PipelineRunCompleted","#,
                r#""pipeline_run_id":"00000000-0000-0000-0000-000000000001","#,
                r#""dataset_id":"00000000-0000-0000-0000-000000000002","#,
                r#""dataset_name":"docs","payload":null,"#,
                r#""data_ingestion_info":[{"run_info":{"status":"PipelineRunCompleted","#,
                r#""pipeline_run_id":"00000000-0000-0000-0000-000000000001","#,
                r#""dataset_id":"00000000-0000-0000-0000-000000000002","#,
                r#""dataset_name":"docs","payload":null,"data_ingestion_info":null},"#,
                r#""data_id":null}],"data_ids":[]}"#,
            )
        );

        // Round-trips, and a Python server's run (with `after_run_error`
        // or other extra keys) reads back to the same ids.
        let back: StageResult = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, stage);
        let mut python = wire;
        python["run"]["after_run_error"] = json!(null);
        let back: StageResult = serde_json::from_value(python).unwrap();
        assert_eq!(back.run, Some(run));
    }

    // A run the ids cannot rebuild reads as no run, never as a completed
    // memify run (a Python server's `review_conflicts` run, an errored run).
    #[test]
    fn a_run_that_is_not_a_completed_memify_run_reads_as_none() {
        let stage = StageResult::completed("review_conflicts").with_run(Some(StageRunInfo {
            pipeline_run_id: Uuid::from_u128(1),
            dataset_id: Uuid::from_u128(2),
            dataset_name: "docs".to_string(),
        }));
        let wire = serde_json::to_value(&stage).unwrap();

        let mut errored = wire.clone();
        errored["run"]["status"] = json!("PipelineRunErrored");
        let back: StageResult = serde_json::from_value(errored.clone()).unwrap();
        assert_eq!(back.run, None);
        assert!(serde_json::from_value::<StageRunInfo>(errored["run"].clone()).is_err());

        let mut with_items = wire.clone();
        with_items["run"]["data_ids"] = json!(["00000000-0000-0000-0000-000000000003"]);
        let back: StageResult = serde_json::from_value(with_items).unwrap();
        assert_eq!(back.run, None);

        let mut malformed = wire;
        malformed["run"] = json!({"status": "PipelineRunCompleted"});
        let back: StageResult = serde_json::from_value(malformed).unwrap();
        assert_eq!(back.run, None);
    }

    #[test]
    fn each_stage_run_gets_a_fresh_run_id() {
        let dataset_id = Uuid::new_v4();
        let first = StageRunInfo::completed(dataset_id, "docs");
        let second = StageRunInfo::completed(dataset_id, "docs");
        assert_ne!(first.pipeline_run_id, second.pipeline_run_id);
        assert_eq!(first.dataset_id, dataset_id);
        assert_eq!(first.dataset_name, "docs");
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
