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

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use tokio::fs;
use uuid::Uuid;

use crate::error::SessionError;
use crate::session_store::SessionStore;
use crate::types::SessionQAEntry;

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
        match fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SessionError::StoreError(format!("fs delete error: {e}"))),
        }
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
}
