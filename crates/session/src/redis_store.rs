//! Redis-backed session store — compatible with Python's `RedisAdapter`.
//!
//! Entries are stored as JSON strings in Redis lists under the key
//! `agent_sessions:{user_id}:{session_id}`, matching the Python SDK format.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use uuid::Uuid;

use crate::error::SessionError;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

/// A single Q&A entry as serialised to/from Redis (Python-compatible JSON).
#[derive(serde::Serialize, serde::Deserialize)]
struct RedisQAEntry {
    time: String,
    question: String,
    context: String,
    answer: String,
    qa_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback_score: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    used_graph_element_ids: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memify_metadata: Option<serde_json::Value>,
}

/// Redis-backed session store.
///
/// Uses the same key format and JSON layout as the Python `RedisAdapter` so
/// both SDKs can share a Redis instance.
pub struct RedisSessionStore {
    conn: MultiplexedConnection,
}

impl RedisSessionStore {
    /// Connect to Redis and return a ready-to-use store.
    pub async fn new(redis_url: &str) -> Result<Self, SessionError> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| SessionError::StoreError(format!("redis client error: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| SessionError::StoreError(format!("redis connection error: {e}")))?;
        Ok(Self { conn })
    }

    /// Build from an already-established connection.
    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn session_key(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("agent_sessions:{uid}:{session_id}")
}

fn build_entry(
    qa_id: &str,
    question: &str,
    answer: &str,
    context: Option<&str>,
    external_event_id: Option<&str>,
) -> RedisQAEntry {
    RedisQAEntry {
        time: Utc::now().to_rfc3339(),
        question: question.to_string(),
        context: context.unwrap_or("").to_string(),
        answer: answer.to_string(),
        qa_id: qa_id.to_string(),
        external_event_id: external_event_id.map(str::to_owned),
        feedback_text: None,
        feedback_score: None,
        used_graph_element_ids: None,
        memify_metadata: None,
    }
}

fn redis_entry_to_domain(e: &RedisQAEntry, session_id: &str) -> SessionQAEntry {
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
        external_event_id: e.external_event_id.clone(),
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

fn graph_context_key(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("graph_knowledge:{uid}:{session_id}")
}

fn trace_key(user_id: &str, session_id: &str) -> String {
    format!("cognee:trace:{user_id}:{session_id}")
}

fn qa_event_key(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("agent_sessions_external_events:{uid}:{session_id}")
}

fn trace_event_key(user_id: &str, session_id: &str) -> String {
    format!("cognee:trace_external_events:{user_id}:{session_id}")
}

const IDEMPOTENT_APPEND_LUA: &str = r#"
local known_id = redis.call('HGET', KEYS[2], ARGV[1])
local entries = redis.call('LRANGE', KEYS[1], 0, -1)
for _, raw in ipairs(entries) do
    local item = cjson.decode(raw)
    if item[ARGV[4]] == ARGV[1] or (known_id and item[ARGV[5]] == known_id) then
        redis.call('HSET', KEYS[2], ARGV[1], item[ARGV[5]])
        return raw
    end
end
if known_id then
    redis.call('HDEL', KEYS[2], ARGV[1])
end
redis.call('RPUSH', KEYS[1], ARGV[2])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
return ARGV[2]
"#;

const CONTAINS_EXTERNAL_EVENT_LUA: &str = r#"
if redis.call('HEXISTS', KEYS[2], ARGV[1]) == 1 then
    return 1
end
local entries = redis.call('LRANGE', KEYS[1], 0, -1)
for _, raw in ipairs(entries) do
    local item = cjson.decode(raw)
    if item.external_event_id == ARGV[1] then
        redis.call('HSET', KEYS[2], ARGV[1], item[ARGV[2]])
        return 1
    end
end
return 0
"#;

fn qa_content_matches(
    entry: &RedisQAEntry,
    question: &str,
    answer: &str,
    context: Option<&str>,
) -> bool {
    entry.question == question
        && entry.answer == answer
        && entry.context == context.unwrap_or_default()
}

fn trace_content_matches(existing: &SessionTraceStep, replay: &SessionTraceStep) -> bool {
    existing.origin_function == replay.origin_function
        && existing.status == replay.status
        && existing.memory_query == replay.memory_query
        && existing.memory_context == replay.memory_context
        && existing.method_params == replay.method_params
        && existing.method_return_value == replay.method_return_value
        && existing.error_message == replay.error_message
        && existing.session_feedback == replay.session_feedback
}

/// Apply a `SessionQAUpdate` to a `RedisQAEntry` in place.
fn apply_update_to_redis_entry(entry: &mut RedisQAEntry, updates: &SessionQAUpdate) {
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

fn map_err(e: redis::RedisError) -> SessionError {
    SessionError::StoreError(format!("redis error: {e}"))
}

// ---------------------------------------------------------------------------
// trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for RedisSessionStore {
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let qa_id = Uuid::new_v4().to_string();
        self.create_qa_entry_with_id(&qa_id, session_id, user_id, question, answer, context, None)
            .await
    }

    async fn create_qa_entry_with_id(
        &self,
        qa_id: &str,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
        external_event_id: Option<&str>,
    ) -> Result<String, SessionError> {
        let entry = build_entry(qa_id, question, answer, context, external_event_id);
        let json = serde_json::to_string(&entry)
            .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;

        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let Some(event_id) = external_event_id else {
            conn.rpush::<_, _, ()>(&key, &json).await.map_err(map_err)?;
            return Ok(qa_id.to_owned());
        };

        let persisted: String = redis::Script::new(IDEMPOTENT_APPEND_LUA)
            .key(&key)
            .key(qa_event_key(user_id, session_id))
            .arg(event_id)
            .arg(&json)
            .arg(qa_id)
            .arg("external_event_id")
            .arg("qa_id")
            .invoke_async(&mut conn)
            .await
            .map_err(map_err)?;
        let persisted: RedisQAEntry = serde_json::from_str(&persisted)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
        if !qa_content_matches(&persisted, question, answer, context) {
            return Err(SessionError::ExternalEventConflict {
                event_id: event_id.to_owned(),
                reason: "Q&A content differs from the persisted entry".into(),
            });
        }

        Ok(persisted.qa_id)
    }

    async fn get_latest_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let raw: Vec<String> = conn
            .lrange(&key, -(last_n as isize), -1)
            .await
            .map_err(map_err)?;

        raw.iter()
            .map(|s| {
                let e: RedisQAEntry = serde_json::from_str(s)
                    .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
                Ok(redis_entry_to_domain(&e, session_id))
            })
            .collect()
    }

    async fn get_all_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;

        raw.iter()
            .map(|s| {
                let e: RedisQAEntry = serde_json::from_str(s)
                    .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
                Ok(redis_entry_to_domain(&e, session_id))
            })
            .collect()
    }

    async fn delete_session(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let event_key = qa_event_key(user_id, session_id);
        let deleted: i64 = conn.del((&key, &event_key)).await.map_err(map_err)?;
        Ok(deleted > 0)
    }

    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();

        // Load all entries, remove the target, rewrite — same approach as Python.
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;

        let mut found = false;
        let mut removed_event_id = None;
        let mut kept = Vec::with_capacity(raw.len());
        for s in &raw {
            let e: RedisQAEntry = serde_json::from_str(s)
                .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
            if e.qa_id == qa_id {
                found = true;
                removed_event_id = e.external_event_id;
            } else {
                kept.push(s.clone());
            }
        }

        if !found {
            return Ok(false);
        }

        conn.del::<_, ()>(&key).await.map_err(map_err)?;
        for entry_json in &kept {
            conn.rpush::<_, _, ()>(&key, entry_json)
                .await
                .map_err(map_err)?;
        }
        if let Some(event_id) = removed_event_id {
            conn.hdel::<_, _, ()>(qa_event_key(user_id, session_id), event_id)
                .await
                .map_err(map_err)?;
        }

        Ok(true)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        let mut conn = self.conn.clone();
        // KEYS is acceptable here because prune() is an infrequent administrative
        // operation, and session keys are typically few in number.
        for pattern in [
            "agent_sessions:*",
            "agent_sessions_external_events:*",
            "graph_knowledge:*",
            "cognee:trace:*",
            "cognee:trace_external_events:*",
        ] {
            let keys: Vec<String> = redis::cmd("KEYS")
                .arg(pattern)
                .query_async(&mut conn)
                .await
                .map_err(map_err)?;
            if !keys.is_empty() {
                conn.del::<_, ()>(&keys).await.map_err(map_err)?;
            }
        }

        Ok(())
    }

    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();

        // Load all entries, find and update the target, rewrite the list.
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;

        let mut found = false;
        let mut new_entries = Vec::with_capacity(raw.len());

        for s in &raw {
            let mut entry: RedisQAEntry = serde_json::from_str(s)
                .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;

            if entry.qa_id == qa_id {
                apply_update_to_redis_entry(&mut entry, &updates);
                found = true;
            }

            let json = serde_json::to_string(&entry)
                .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
            new_entries.push(json);
        }

        if !found {
            return Ok(false);
        }

        // Rewrite the list atomically
        conn.del::<_, ()>(&key).await.map_err(map_err)?;
        for entry_json in &new_entries {
            conn.rpush::<_, _, ()>(&key, entry_json)
                .await
                .map_err(map_err)?;
        }

        Ok(true)
    }

    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let key = graph_context_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let result: Option<String> = conn.get(&key).await.map_err(map_err)?;
        Ok(result)
    }

    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        let key = graph_context_key(user_id, session_id);
        let mut conn = self.conn.clone();
        conn.set::<_, _, ()>(&key, context).await.map_err(map_err)?;
        Ok(())
    }

    async fn save_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let trace_id = step.trace_id.clone();
        let json = serde_json::to_string(&step)
            .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;

        // Python uses RPUSH (NOT LPUSH) so LRANGE 0 -1 returns oldest-first.
        let key = trace_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let Some(event_id) = step.external_event_id.as_deref() else {
            conn.rpush::<_, _, ()>(&key, &json).await.map_err(map_err)?;
            return Ok(trace_id);
        };

        let persisted: String = redis::Script::new(IDEMPOTENT_APPEND_LUA)
            .key(&key)
            .key(trace_event_key(user_id, session_id))
            .arg(event_id)
            .arg(&json)
            .arg(&trace_id)
            .arg("external_event_id")
            .arg("trace_id")
            .invoke_async(&mut conn)
            .await
            .map_err(map_err)?;
        let persisted: SessionTraceStep = serde_json::from_str(&persisted)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
        if !trace_content_matches(&persisted, &step) {
            return Err(SessionError::ExternalEventConflict {
                event_id: event_id.to_owned(),
                reason: "trace content differs from the persisted entry".into(),
            });
        }

        Ok(persisted.trace_id)
    }

    async fn read_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let key = trace_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;

        raw.iter()
            .map(|s| {
                serde_json::from_str::<SessionTraceStep>(s)
                    .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))
            })
            .collect()
    }

    async fn contains_external_event(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        external_event_id: &str,
    ) -> Result<bool, SessionError> {
        let mut conn = self.conn.clone();
        let qa_found: i64 = redis::Script::new(CONTAINS_EXTERNAL_EVENT_LUA)
            .key(session_key(user_id, session_id))
            .key(qa_event_key(user_id, session_id))
            .arg(external_event_id)
            .arg("qa_id")
            .invoke_async(&mut conn)
            .await
            .map_err(map_err)?;
        if qa_found == 1 {
            return Ok(true);
        }
        let Some(user_id) = user_id else {
            return Ok(false);
        };
        let trace_found: i64 = redis::Script::new(CONTAINS_EXTERNAL_EVENT_LUA)
            .key(trace_key(user_id, session_id))
            .key(trace_event_key(user_id, session_id))
            .arg(external_event_id)
            .arg("trace_id")
            .invoke_async(&mut conn)
            .await
            .map_err(map_err)?;
        Ok(trace_found == 1)
    }
}
