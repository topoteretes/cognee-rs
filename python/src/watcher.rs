use async_trait::async_trait;
use pyo3::prelude::*;
use uuid::Uuid;

use cognee_core::pipeline::{PipelineRunInfo, PipelineStatus, PipelineWatcher, TaskStatus};

/// Bridges a Python duck-typed watcher object to `PipelineWatcher`.
///
/// Each trait method checks whether the Python object has the corresponding
/// method (via `hasattr`) and calls it if present. Missing methods are no-ops.
pub struct PyWatcherBridge {
    inner: Py<PyAny>,
}

impl PyWatcherBridge {
    pub fn new(obj: Py<PyAny>) -> Self {
        Self { inner: obj }
    }
}

#[async_trait]
impl PipelineWatcher for PyWatcherBridge {
    async fn on_pipeline(&self, pipeline_id: Uuid, status: PipelineStatus) {
        let status_str = pipeline_status_to_string(&status);
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_pipeline") {
                let _ = self.inner.call_method1(
                    py,
                    "on_pipeline",
                    (pipeline_id.to_string(), status_str),
                );
            }
        });
    }

    async fn on_task(
        &self,
        pipeline_id: Uuid,
        task_index: usize,
        task_name: Option<&str>,
        total_tasks: usize,
        status: TaskStatus,
    ) {
        let status_str = task_status_to_string(&status);
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_task") {
                let _ = self.inner.call_method1(
                    py,
                    "on_task",
                    (
                        pipeline_id.to_string(),
                        task_index,
                        task_name.unwrap_or(""),
                        total_tasks,
                        status_str,
                    ),
                );
            }
        });
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_run_started") {
                let _ = self.inner.call_method1(
                    py,
                    "on_run_started",
                    (run.run_id.to_string(), run.pipeline_name.clone()),
                );
            }
        });
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, output_count: usize) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_run_completed") {
                let _ = self.inner.call_method1(
                    py,
                    "on_run_completed",
                    (run.run_id.to_string(), output_count),
                );
            }
        });
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_run_errored") {
                let _ = self.inner.call_method1(
                    py,
                    "on_run_errored",
                    (run.run_id.to_string(), error.to_string()),
                );
            }
        });
    }

    async fn on_task_started(&self, run: &PipelineRunInfo, task_name: &str, task_index: usize) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_task_started") {
                let _ = self.inner.call_method1(
                    py,
                    "on_task_started",
                    (run.run_id.to_string(), task_name.to_string(), task_index),
                );
            }
        });
    }

    async fn on_task_completed(&self, run: &PipelineRunInfo, task_name: &str, output_count: usize) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_task_completed") {
                let _ = self.inner.call_method1(
                    py,
                    "on_task_completed",
                    (run.run_id.to_string(), task_name.to_string(), output_count),
                );
            }
        });
    }

    async fn on_task_errored(&self, run: &PipelineRunInfo, task_name: &str, error: &str) {
        Python::with_gil(|py| {
            if let Ok(true) = self.inner.bind(py).hasattr("on_task_errored") {
                let _ = self.inner.call_method1(
                    py,
                    "on_task_errored",
                    (
                        run.run_id.to_string(),
                        task_name.to_string(),
                        error.to_string(),
                    ),
                );
            }
        });
    }
}

fn pipeline_status_to_string(status: &PipelineStatus) -> String {
    match status {
        PipelineStatus::Started { task_count } => format!("started({task_count} tasks)"),
        PipelineStatus::Succeeded { output_count } => format!("succeeded({output_count} outputs)"),
        PipelineStatus::Failed { task_index, error } => {
            format!("failed(task {task_index}: {error})")
        }
        PipelineStatus::Cancelled => "cancelled".to_string(),
        PipelineStatus::ItemSkipped { data_id } => format!("item_skipped({data_id})"),
    }
}

fn task_status_to_string(status: &TaskStatus) -> String {
    match status {
        TaskStatus::Started => "started".to_string(),
        TaskStatus::Retrying { attempt, error } => format!("retrying(attempt {attempt}: {error})"),
        TaskStatus::Succeeded => "succeeded".to_string(),
        TaskStatus::Failed { attempts, error } => {
            format!("failed({attempts} attempts: {error})")
        }
    }
}
