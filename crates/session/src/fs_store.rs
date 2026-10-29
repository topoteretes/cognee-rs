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

use crate::context_entry::SessionContextRow;
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
    /// How long a session file lives after its last write; `None` never
    /// expires.
    session_ttl: Option<std::time::Duration>,
}

impl FsSessionStore {
    /// Create a new filesystem store rooted at `base_dir`.
    ///
    /// The directory is created on first write if it does not exist.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            session_ttl: None,
        }
    }

    /// Expire a session's files (Q&A entries, trace steps, session-context
    /// rows)
    /// this many seconds after their last write, as Python's `FSCacheAdapter`
    /// does with `session_ttl_seconds` (its `diskcache` expiry, refreshed on
    /// every write). An expired file is deleted when it is next read. `0`
    /// never expires them, the behaviour of a store built without this call.
    #[must_use]
    pub fn with_session_ttl_seconds(mut self, seconds: u64) -> Self {
        self.session_ttl = (seconds > 0).then(|| std::time::Duration::from_secs(seconds));
        self
    }

    /// Delete `path` if it was last written longer ago than the session TTL,
    /// so the read that follows finds nothing.
    async fn expire(&self, path: &Path) -> Result<(), SessionError> {
        let Some(ttl) = self.session_ttl else {
            return Ok(());
        };
        let modified = match fs::metadata(path).await.and_then(|m| m.modified()) {
            Ok(modified) => modified,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(SessionError::StoreError(format!("fs stat error: {e}"))),
        };
        // A clock that went backwards reads as "just written".
        let age = modified.elapsed().unwrap_or_default();
        if age > ttl {
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(SessionError::StoreError(format!("fs delete error: {e}"))),
            }
        }
        Ok(())
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

/// The graph-knowledge snapshot file of the removed graph-to-session sync.
/// Only `delete_session` still touches it, to clean up files written before
/// the removal (Python keeps the same one-release cleanup).
fn legacy_graph_context_file(base: &Path, user_id: Option<&str>, session_id: &str) -> PathBuf {
    let uid = user_id.unwrap_or("default");
    base.join(uid)
        .join(format!("_graph_context_{session_id}.json"))
}

fn trace_session_file(base: &Path, user_id: &str, session_id: &str) -> PathBuf {
    base.join(user_id).join(format!("{session_id}.traces.json"))
}

fn context_session_file(base: &Path, user_id: Option<&str>, session_id: &str) -> PathBuf {
    let uid = user_id.unwrap_or("default");
    base.join(uid).join(format!("{session_id}.context.json"))
}

/// Load a JSON array file (trace steps, session-context rows); a missing or
/// empty file is an empty list.
async fn load_json_list<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Vec<T>, SessionError> {
    match fs::read_to_string(path).await {
        Ok(contents) if !contents.is_empty() => serde_json::from_str(&contents)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}"))),
        Ok(_) => Ok(vec![]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(SessionError::StoreError(format!("fs read error: {e}"))),
    }
}

async fn save_json_list<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| SessionError::StoreError(format!("fs mkdir error: {e}")))?;
    }
    let json = serde_json::to_string_pretty(items)
        .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
    fs::write(path, json)
        .await
        .map_err(|e| SessionError::StoreError(format!("fs write error: {e}")))?;
    Ok(())
}

/// Remove `path`; whether it existed.
async fn remove_if_exists(path: &Path) -> Result<bool, SessionError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(SessionError::StoreError(format!("fs delete error: {e}"))),
    }
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
            .map(|v| crate::types::merge_memify_metadata(entry.memify_metadata.take(), v));
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

        self.expire(&path).await?;
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
        self.expire(&path).await?;
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
        self.expire(&path).await?;
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
        let existed = remove_if_exists(&path).await?;

        // Also remove a legacy graph-context snapshot (best-effort).
        let gc_path = legacy_graph_context_file(&self.base_dir, user_id, session_id);
        let _ = fs::remove_file(&gc_path).await; // ignore NotFound

        // And its session-context rows, which count toward "existed" as in
        // Python's `delete_session`.
        let ctx_path = context_session_file(&self.base_dir, user_id, session_id);
        let context_existed = remove_if_exists(&ctx_path).await?;

        Ok(existed || context_existed)
    }

    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        let path = session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
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
        self.expire(&path).await?;
        let mut entries = load_entries(&path).await?;

        let Some(entry) = entries.iter_mut().find(|e| e.qa_id == qa_id) else {
            return Ok(false);
        };

        apply_update_to_fs_entry(entry, &updates);
        save_entries(&path, &entries).await?;
        Ok(true)
    }

    async fn save_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let path = trace_session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
        let mut steps = load_json_list(&path).await?;
        let trace_id = step.trace_id.clone();
        steps.push(step);
        save_json_list(&path, &steps).await?;
        Ok(trace_id)
    }

    async fn read_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let path = trace_session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
        load_json_list(&path).await
    }

    async fn create_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry: SessionContextRow,
    ) -> Result<(), SessionError> {
        let path = context_session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
        let mut rows: Vec<SessionContextRow> = load_json_list(&path).await?;
        rows.push(entry);
        save_json_list(&path, &rows).await
    }

    async fn get_session_context_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionContextRow>, SessionError> {
        let path = context_session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
        load_json_list(&path).await
    }

    async fn update_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry_id: &str,
        merge: SessionContextRow,
    ) -> Result<bool, SessionError> {
        let path = context_session_file(&self.base_dir, user_id, session_id);
        self.expire(&path).await?;
        let mut rows: Vec<SessionContextRow> = load_json_list(&path).await?;
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.get("id").and_then(serde_json::Value::as_str) == Some(entry_id))
        else {
            return Ok(false);
        };
        row.extend(merge);
        save_json_list(&path, &rows).await?;
        Ok(true)
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

    /// Push `path`'s last write `age` into the past.
    fn age_file(path: &Path, age: std::time::Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    /// With a TTL, a session file not written for longer than it reads as
    /// gone (and is deleted); a fresh one, or any file without a TTL, stays.
    #[tokio::test]
    async fn session_ttl_expires_files_not_written_within_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSessionStore::new(dir.path()).with_session_ttl_seconds(60);
        store
            .create_qa_entry("s", Some("u"), "q", "a", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .get_all_qa_entries("s", Some("u"))
                .await
                .unwrap()
                .len(),
            1
        );

        let qa = session_file(dir.path(), Some("u"), "s");
        age_file(&qa, std::time::Duration::from_secs(120));

        let untimed = FsSessionStore::new(dir.path());
        assert_eq!(
            untimed
                .get_all_qa_entries("s", Some("u"))
                .await
                .unwrap()
                .len(),
            1,
            "without a TTL nothing expires"
        );

        assert!(
            store
                .get_all_qa_entries("s", Some("u"))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!qa.exists(), "the expired file is deleted");
    }

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

        // A second write overlays its own keys (Python's `update_qa` merge).
        store
            .update_qa_entry(
                "sess1",
                None,
                &qa_id,
                SessionQAUpdate {
                    memify_metadata: Some(Some(HashMap::from([(
                        "preference_turn_counted".to_string(),
                        true,
                    )]))),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");
        let entries = store
            .get_all_qa_entries("sess1", None)
            .await
            .expect("get should succeed");
        let mm = entries[0].memify_metadata.as_ref().expect("metadata");
        assert_eq!(mm.get("feedback_weights_applied"), Some(&false));
        assert_eq!(mm.get("preference_turn_counted"), Some(&true));
    }

    /// A graph-context snapshot left by the removed graph-to-session sync is
    /// deleted with its session.
    #[tokio::test]
    async fn delete_session_also_removes_a_legacy_graph_context_file() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let store = FsSessionStore::new(dir.path());

        store
            .create_qa_entry("sess1", None, "q1?", "a1", None)
            .await
            .expect("create should succeed");
        let legacy = legacy_graph_context_file(dir.path(), None, "sess1");
        std::fs::write(&legacy, "\"some context\"").expect("write legacy file");

        let deleted = store
            .delete_session("sess1", None)
            .await
            .expect("delete should succeed");
        assert!(deleted);
        assert!(!legacy.exists(), "the legacy snapshot is removed");
    }
}
