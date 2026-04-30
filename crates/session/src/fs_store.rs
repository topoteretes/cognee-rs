//! Filesystem-backed session store — compatible with Python's `FSCacheAdapter`.
//!
//! Each session is stored as a JSON file containing an array of Q&A entries.
//! The file path mirrors the Python key format:
//! `{base_dir}/{user_id}/{session_id}.json`
//!
//! Python uses `diskcache` which stores values in a SQLite database under
//! `.cognee_fs_cache/sessions_db/`. Our implementation uses plain JSON files
//! for simplicity while keeping the same logical key structure
//! (`agent_sessions:{user_id}:{session_id}`) and identical JSON entry format
//! so data written by one SDK can be read by the other at the API level.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use tokio::fs;
use uuid::Uuid;

use crate::error::SessionError;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

/// JSON layout of a single entry on disk — matches the Python `SessionQAEntry`
/// model fields so the serialised form is cross-SDK compatible.
#[derive(serde::Serialize, serde::Deserialize)]
struct FsQAEntry {
    time: String,
    question: String,
    context: String,
    answer: String,
    qa_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback_score: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    used_graph_element_ids: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memify_metadata: Option<serde_json::Value>,
}

/// Filesystem-backed session store.
pub struct FsSessionStore {
    base_dir: PathBuf,
}

impl FsSessionStore {
    /// Create a new filesystem store rooted at `base_dir`.
    ///
    /// The directory is created on first write if it does not exist.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn session_file(base: &Path, user_id: Option<&str>, session_id: &str) -> PathBuf {
    let uid = user_id.unwrap_or("default");
    base.join(uid).join(format!("{session_id}.json"))
}

async fn load_entries(path: &Path) -> Result<Vec<FsQAEntry>, SessionError> {
    match fs::read_to_string(path).await {
        Ok(contents) if !contents.is_empty() => serde_json::from_str(&contents)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}"))),
        Ok(_) => Ok(vec![]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(SessionError::StoreError(format!("fs read error: {e}"))),
    }
}

async fn save_entries(path: &Path, entries: &[FsQAEntry]) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| SessionError::StoreError(format!("fs mkdir error: {e}")))?;
    }
    let json = serde_json::to_string_pretty(entries)
        .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
    fs::write(path, json)
        .await
        .map_err(|e| SessionError::StoreError(format!("fs write error: {e}")))?;
    Ok(())
}

fn build_entry(qa_id: &str, question: &str, answer: &str, context: Option<&str>) -> FsQAEntry {
    FsQAEntry {
        time: Utc::now().to_rfc3339(),
        question: question.to_string(),
        context: context.unwrap_or("").to_string(),
        answer: answer.to_string(),
        qa_id: qa_id.to_string(),
        feedback_text: None,
        feedback_score: None,
        used_graph_element_ids: None,
        memify_metadata: None,
    }
}

fn fs_entry_to_domain(e: &FsQAEntry, session_id: &str) -> SessionQAEntry {
    let used_graph_element_ids = e
        .used_graph_element_ids
        .as_ref()
        .and_then(|v| serde_json::from_value::<UsedGraphElementIds>(v.clone()).ok());
    let memify_metadata = e
        .memify_metadata
        .as_ref()
        .and_then(|v| serde_json::from_value::<HashMap<String, bool>>(v.clone()).ok());

    SessionQAEntry {
        id: Uuid::parse_str(&e.qa_id).unwrap_or_else(|_| Uuid::new_v4()),
        session_id: session_id.to_string(),
        user_id: None,
        question: e.question.clone(),
        answer: e.answer.clone(),
        context: if e.context.is_empty() {
            None
        } else {
            Some(e.context.clone())
        },
        created_at: chrono::DateTime::parse_from_rfc3339(&e.time)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        feedback_text: e.feedback_text.clone(),
        feedback_score: e.feedback_score,
        used_graph_element_ids,
        memify_metadata,
    }
}

fn graph_context_file(base: &Path, user_id: Option<&str>, session_id: &str) -> PathBuf {
    let uid = user_id.unwrap_or("default");
    base.join(uid)
        .join(format!("_graph_context_{session_id}.json"))
}

fn trace_session_file(base: &Path, user_id: &str, session_id: &str) -> PathBuf {
    base.join(user_id).join(format!("{session_id}.traces.json"))
}

async fn load_trace_steps(path: &Path) -> Result<Vec<SessionTraceStep>, SessionError> {
    match fs::read_to_string(path).await {
        Ok(contents) if !contents.is_empty() => serde_json::from_str(&contents)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}"))),
        Ok(_) => Ok(vec![]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(SessionError::StoreError(format!("fs read error: {e}"))),
    }
}

async fn save_trace_steps(path: &Path, steps: &[SessionTraceStep]) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| SessionError::StoreError(format!("fs mkdir error: {e}")))?;
    }
    let json = serde_json::to_string_pretty(steps)
        .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
    fs::write(path, json)
        .await
        .map_err(|e| SessionError::StoreError(format!("fs write error: {e}")))?;
    Ok(())
}

/// Apply a `SessionQAUpdate` to an `FsQAEntry` in place.
fn apply_update_to_fs_entry(entry: &mut FsQAEntry, updates: &SessionQAUpdate) {
    if let Some(ref q) = updates.question {
        entry.question = q.clone();
    }
    if let Some(ref a) = updates.answer {
        entry.answer = a.clone();
    }
    if let Some(ref ctx) = updates.context {
        entry.context = ctx.as_deref().unwrap_or("").to_string();
    }
    if let Some(ref ft) = updates.feedback_text {
        entry.feedback_text = ft.clone();
    }
    if let Some(ref fs) = updates.feedback_score {
        entry.feedback_score = *fs;
    }
    if let Some(ref ids) = updates.used_graph_element_ids {
        entry.used_graph_element_ids = ids
            .as_ref()
            .map(|v| serde_json::to_value(v).unwrap_or_default());
    }
    if let Some(ref mm) = updates.memify_metadata {
        entry.memify_metadata = mm
            .as_ref()
            .map(|v| serde_json::to_value(v).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------
// trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for FsSessionStore {
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let qa_id = Uuid::new_v4().to_string();
        let entry = build_entry(&qa_id, question, answer, context);

        let mut entries = load_entries(&path).await?;
        entries.push(entry);
        save_entries(&path, &entries).await?;

        Ok(qa_id)
    }

    async fn get_latest_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let entries = load_entries(&path).await?;

        let start = entries.len().saturating_sub(last_n);
        Ok(entries[start..]
            .iter()
            .map(|e| fs_entry_to_domain(e, session_id))
            .collect())
    }

    async fn get_all_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let entries = load_entries(&path).await?;
        Ok(entries
            .iter()
            .map(|e| fs_entry_to_domain(e, session_id))
            .collect())
    }

    async fn delete_session(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let existed = match fs::remove_file(&path).await {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(SessionError::StoreError(format!("fs delete error: {e}"))),
        };

        // Also remove the graph context file for this session (best-effort).
        let gc_path = graph_context_file(&self.base_dir, user_id, session_id);
        let _ = fs::remove_file(&gc_path).await; // ignore NotFound

        Ok(existed)
    }

    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let mut entries = load_entries(&path).await?;
        let before = entries.len();
        entries.retain(|e| e.qa_id != qa_id);

        if entries.len() == before {
            return Ok(false);
        }

        if entries.is_empty() {
            // Clean up empty file
            let _ = fs::remove_file(&path).await;
        } else {
            save_entries(&path, &entries).await?;
        }

        Ok(true)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        // Remove the entire base directory and recreate it empty.
        // Errors are ignored when the directory does not exist yet.
        match fs::remove_dir_all(&self.base_dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(SessionError::StoreError(format!(
                    "fs prune remove_dir_all error: {e}"
                )));
            }
        }
        fs::create_dir_all(&self.base_dir)
            .await
            .map_err(|e| SessionError::StoreError(format!("fs prune create_dir_all error: {e}")))?;
        Ok(())
    }

    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        let mut entries = load_entries(&path).await?;

        let Some(entry) = entries.iter_mut().find(|e| e.qa_id == qa_id) else {
            return Ok(false);
        };

        apply_update_to_fs_entry(entry, &updates);
        save_entries(&path, &entries).await?;
        Ok(true)
    }

    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let path = graph_context_file(&self.base_dir, user_id, session_id);
        match fs::read_to_string(&path).await {
            Ok(contents) if !contents.is_empty() => {
                // Stored as a JSON string
                let ctx: String = serde_json::from_str(&contents)
                    .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
                Ok(Some(ctx))
            }
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SessionError::StoreError(format!("fs read error: {e}"))),
        }
    }

    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        let path = graph_context_file(&self.base_dir, user_id, session_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| SessionError::StoreError(format!("fs mkdir error: {e}")))?;
        }
        let json = serde_json::to_string_pretty(context)
            .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
        fs::write(&path, json)
            .await
            .map_err(|e| SessionError::StoreError(format!("fs write error: {e}")))?;
        Ok(())
    }

    async fn save_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let path = trace_session_file(&self.base_dir, user_id, session_id);
        let mut steps = load_trace_steps(&path).await?;
        let trace_id = step.trace_id.clone();
        steps.push(step);
        save_trace_steps(&path, &steps).await?;
        Ok(trace_id)
    }

    async fn read_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let path = trace_session_file(&self.base_dir, user_id, session_id);
        load_trace_steps(&path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_read_entry_has_no_feedback() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        let qa_id = store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id.to_string(), qa_id);
        assert!(entries[0].feedback_text.is_none());
        assert!(entries[0].feedback_score.is_none());
        assert!(entries[0].used_graph_element_ids.is_none());
        assert!(entries[0].memify_metadata.is_none());
    }

    #[tokio::test]
    async fn update_qa_entry_sets_feedback_fields() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        let qa_id = store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        let updated = store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    feedback_text: Some(Some("Great answer!".to_string())),
                    feedback_score: Some(Some(5)),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        assert!(updated);

        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");

        assert_eq!(entries[0].feedback_text.as_deref(), Some("Great answer!"));
        assert_eq!(entries[0].feedback_score, Some(5));
    }

    #[tokio::test]
    async fn update_qa_entry_clears_feedback() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        let qa_id = store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        // Set feedback
        store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    feedback_text: Some(Some("Good".to_string())),
                    feedback_score: Some(Some(4)),
                    ..Default::default()
                },
            )
            .await
            .expect("set should succeed");

        // Clear feedback
        store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    feedback_text: Some(None),
                    feedback_score: Some(None),
                    ..Default::default()
                },
            )
            .await
            .expect("clear should succeed");

        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");

        assert!(entries[0].feedback_text.is_none());
        assert!(entries[0].feedback_score.is_none());
    }

    #[tokio::test]
    async fn update_qa_entry_not_found() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        let updated = store
            .update_qa_entry("sess1", None, "nonexistent", SessionQAUpdate::default())
            .await
            .expect("update call should succeed even when not found");

        assert!(!updated);
    }

    #[tokio::test]
    async fn graph_context_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        // Initially no context
        let ctx = store
            .get_graph_context("sess1", None)
            .await
            .expect("get should succeed");
        assert!(ctx.is_none());

        // Set context
        store
            .set_graph_context("sess1", None, "Some graph knowledge")
            .await
            .expect("set should succeed");

        // Read it back
        let ctx = store
            .get_graph_context("sess1", None)
            .await
            .expect("get should succeed");
        assert_eq!(ctx.as_deref(), Some("Some graph knowledge"));

        // Overwrite
        store
            .set_graph_context("sess1", None, "Updated graph knowledge")
            .await
            .expect("set should succeed");

        let ctx = store
            .get_graph_context("sess1", None)
            .await
            .expect("get should succeed");
        assert_eq!(ctx.as_deref(), Some("Updated graph knowledge"));
    }

    #[tokio::test]
    async fn update_qa_entry_with_graph_element_ids() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        let qa_id = store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        let ids = UsedGraphElementIds {
            node_ids: vec!["node-1".to_string(), "node-2".to_string()],
            edge_ids: vec!["edge-1".to_string()],
        };

        store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    used_graph_element_ids: Some(Some(ids)),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");

        let graph_ids = entries[0]
            .used_graph_element_ids
            .as_ref()
            .expect("should have graph element ids");
        assert_eq!(graph_ids.node_ids, vec!["node-1", "node-2"]);
        assert_eq!(graph_ids.edge_ids, vec!["edge-1"]);
    }

    #[tokio::test]
    async fn update_qa_entry_with_memify_metadata() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        let qa_id = store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        let mut meta = HashMap::new();
        meta.insert("feedback_weights_applied".to_string(), false);

        store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    memify_metadata: Some(Some(meta)),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");

        let mm = entries[0]
            .memify_metadata
            .as_ref()
            .expect("should have memify metadata");
        assert_eq!(mm.get("feedback_weights_applied"), Some(&false));
    }

    #[tokio::test]
    async fn delete_session_also_removes_graph_context() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");

        store
            .set_graph_context("sess1", None, "some context")
            .await
            .expect("set should succeed");

        let deleted = store
            .delete_session("sess1", None)
            .await
            .expect("delete should succeed");
        assert!(deleted);

        let ctx = store
            .get_graph_context("sess1", None)
            .await
            .expect("get should succeed");
        assert!(ctx.is_none());
    }
}
