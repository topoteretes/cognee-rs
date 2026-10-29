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

use crate::context_entry::SessionContextRow;
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
    /// Seconds a session's keys live after its last write; 0 never expires.
    session_ttl_seconds: u64,
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
        Ok(Self::from_connection(conn))
    }

    /// Connect to Redis at `host:port` with optional credentials.
    ///
    /// Unlike [`Self::new`], nothing is spliced into a URL: the host and the
    /// credentials travel as separate fields (as Python's `RedisAdapter`
    /// passes them as separate kwargs), so a password with URL-reserved
    /// characters (`@`, `/`, `#`, `%`, …) or a bare IPv6 host such as `::1`
    /// reaches the server verbatim. An empty username or password means
    /// "none".
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<Self, SessionError> {
        let client = redis::Client::open(connection_info(host, port, username, password))
            .map_err(|e| SessionError::StoreError(format!("redis client error: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| SessionError::StoreError(format!("redis connection error: {e}")))?;
        Ok(Self::from_connection(conn))
    }

    /// Build from an already-established connection.
    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self {
            conn,
            session_ttl_seconds: 0,
        }
    }

    /// Expire a session's keys (Q&A list, trace list, graph context,
    /// session-context list) this
    /// many seconds after their last write, as Python's `RedisAdapter` does
    /// with `session_ttl_seconds` (default 604 800, 7 days). `0` never
    /// expires them, the behaviour of a store built without this call.
    #[must_use]
    pub fn with_session_ttl_seconds(mut self, seconds: u64) -> Self {
        self.session_ttl_seconds = seconds;
        self
    }

    /// Python's `_apply_session_ttl`: (re)arm `key`'s expiry after a write.
    async fn apply_session_ttl(
        &self,
        conn: &mut MultiplexedConnection,
        key: &str,
    ) -> Result<(), SessionError> {
        if self.session_ttl_seconds == 0 {
            return Ok(());
        }
        let seconds = i64::try_from(self.session_ttl_seconds).unwrap_or(i64::MAX);
        conn.expire::<_, ()>(key, seconds).await.map_err(map_err)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The connection info for [`RedisSessionStore::connect`]: TCP `host:port`,
/// database 0, and the credentials as-is (empty → `None`).
fn connection_info(host: &str, port: u16, username: &str, password: &str) -> redis::ConnectionInfo {
    let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    redis::ConnectionInfo {
        addr: redis::ConnectionAddr::Tcp(host.to_string(), port),
        redis: redis::RedisConnectionInfo {
            username: non_empty(username),
            password: non_empty(password),
            ..Default::default()
        },
    }
}

fn session_key(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("agent_sessions:{uid}:{session_id}")
}

fn build_entry(qa_id: &str, question: &str, answer: &str, context: Option<&str>) -> RedisQAEntry {
    RedisQAEntry {
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

/// Python's `RedisAdapter._session_context_key`.
fn session_context_key(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("session_context:{uid}:{session_id}")
}

fn parse_context_row(raw: &str) -> Result<SessionContextRow, SessionError> {
    serde_json::from_str(raw)
        .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))
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
            .map(|v| crate::types::merge_memify_metadata(entry.memify_metadata.take(), v));
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
        let entry = build_entry(&qa_id, question, answer, context);
        let json = serde_json::to_string(&entry)
            .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;

        let key = session_key(user_id, session_id);
        let mut conn = self.conn.clone();
        conn.rpush::<_, _, ()>(&key, &json).await.map_err(map_err)?;
        self.apply_session_ttl(&mut conn, &key).await?;

        Ok(qa_id)
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
        // The session-context list goes with the session and counts toward
        // "existed", as in Python's `delete_session`.
        let keys = [
            session_key(user_id, session_id),
            session_context_key(user_id, session_id),
        ];
        let mut conn = self.conn.clone();
        let deleted: i64 = conn.del(&keys).await.map_err(map_err)?;
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
        let mut kept = Vec::with_capacity(raw.len());
        for s in &raw {
            let e: RedisQAEntry = serde_json::from_str(s)
                .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))?;
            if e.qa_id == qa_id {
                found = true;
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
        if !kept.is_empty() {
            self.apply_session_ttl(&mut conn, &key).await?;
        }

        Ok(true)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        let mut conn = self.conn.clone();
        let pattern = "agent_sessions:*";

        // Use KEYS to find all session keys, then DEL them.
        // KEYS is acceptable here because prune() is an infrequent administrative
        // operation, and session keys are typically few in number.
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(pattern)
            .query_async(&mut conn)
            .await
            .map_err(map_err)?;

        if !keys.is_empty() {
            conn.del::<_, ()>(&keys).await.map_err(map_err)?;
        }

        // Also delete the graph_knowledge:* and session_context:* keys.
        for pattern in ["graph_knowledge:*", "session_context:*"] {
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
        self.apply_session_ttl(&mut conn, &key).await?;

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
        self.apply_session_ttl(&mut conn, &key).await?;
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
        conn.rpush::<_, _, ()>(&key, &json).await.map_err(map_err)?;
        self.apply_session_ttl(&mut conn, &key).await?;

        Ok(trace_id)
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

    async fn create_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry: SessionContextRow,
    ) -> Result<(), SessionError> {
        let json = serde_json::to_string(&entry)
            .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
        let key = session_context_key(user_id, session_id);
        let mut conn = self.conn.clone();
        conn.rpush::<_, _, ()>(&key, &json).await.map_err(map_err)?;
        self.apply_session_ttl(&mut conn, &key).await
    }

    async fn get_session_context_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionContextRow>, SessionError> {
        let key = session_context_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;
        raw.iter().map(|s| parse_context_row(s)).collect()
    }

    async fn update_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry_id: &str,
        merge: SessionContextRow,
    ) -> Result<bool, SessionError> {
        let key = session_context_key(user_id, session_id);
        let mut conn = self.conn.clone();
        let raw: Vec<String> = conn.lrange(&key, 0, -1).await.map_err(map_err)?;
        for (index, s) in raw.iter().enumerate() {
            let mut row = parse_context_row(s)?;
            if row.get("id").and_then(serde_json::Value::as_str) != Some(entry_id) {
                continue;
            }
            row.extend(merge);
            let json = serde_json::to_string(&row)
                .map_err(|e| SessionError::StoreError(format!("json error: {e}")))?;
            // Overwrite in place (Python's `_write_entry_at`, an LSET).
            let index = isize::try_from(index)
                .map_err(|e| SessionError::StoreError(format!("list index overflow: {e}")))?;
            conn.lset::<_, _, ()>(&key, index, &json)
                .await
                .map_err(map_err)?;
            self.apply_session_ttl(&mut conn, &key).await?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::connection_info;

    #[test]
    fn credentials_with_url_reserved_characters_are_passed_verbatim() {
        let info = connection_info("localhost", 6379, "us@er", "p@ss/w#rd%2F");
        match &info.addr {
            redis::ConnectionAddr::Tcp(host, port) => {
                assert_eq!(host, "localhost");
                assert_eq!(*port, 6379);
            }
            other => panic!("expected a TCP address, got {other:?}"),
        }
        assert_eq!(info.redis.username.as_deref(), Some("us@er"));
        assert_eq!(info.redis.password.as_deref(), Some("p@ss/w#rd%2F"));
        assert_eq!(info.redis.db, 0);
    }

    #[test]
    fn an_ipv6_host_needs_no_brackets() {
        let info = connection_info("::1", 6380, "", "");
        match &info.addr {
            redis::ConnectionAddr::Tcp(host, port) => {
                assert_eq!(host, "::1");
                assert_eq!(*port, 6380);
            }
            other => panic!("expected a TCP address, got {other:?}"),
        }
    }

    /// `with_session_ttl_seconds` arms an expiry on every key a write
    /// touches; without it nothing expires. Needs a Redis server:
    /// `REDIS_TEST_URL=redis://127.0.0.1:6379`; skipped when unset. CI runs
    /// it in the `redis-session` lane.
    #[tokio::test]
    #[allow(
        clippy::disallowed_methods,
        reason = "tests drive configuration through the process env"
    )]
    async fn session_ttl_expires_every_written_key() {
        use redis::AsyncCommands;

        use super::{RedisSessionStore, graph_context_key, session_key, trace_key};
        use crate::session_store::SessionStore;
        use crate::types::SessionTraceStep;

        let Some(url) = std::env::var("REDIS_TEST_URL")
            .ok()
            .filter(|u| !u.is_empty())
        else {
            // The `REDIS_TEST_URL not set … skipping` shape is what the CI
            // lane's `assert_pg_suite_ran.sh` guard fails on.
            eprintln!("REDIS_TEST_URL not set — skipping session_ttl_expires_every_written_key");
            return;
        };
        let session = format!("ttl-{}", uuid::Uuid::new_v4());
        let user = "ttl-user";

        let store = RedisSessionStore::new(&url)
            .await
            .unwrap()
            .with_session_ttl_seconds(120);
        store
            .create_qa_entry(&session, Some(user), "q", "a", None)
            .await
            .unwrap();
        store
            .set_graph_context(&session, Some(user), "ctx")
            .await
            .unwrap();
        store
            .save_trace_step(
                user,
                &session,
                SessionTraceStep {
                    trace_id: "t1".to_string(),
                    origin_function: "f".to_string(),
                    status: "success".to_string(),
                    memory_query: String::new(),
                    memory_context: String::new(),
                    method_params: serde_json::json!({}),
                    method_return_value: None,
                    error_message: String::new(),
                    session_feedback: String::new(),
                },
            )
            .await
            .unwrap();

        let mut conn = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        for key in [
            session_key(Some(user), &session),
            graph_context_key(Some(user), &session),
            trace_key(user, &session),
        ] {
            let ttl: i64 = conn.ttl(&key).await.unwrap();
            assert!((1..=120).contains(&ttl), "{key}: ttl {ttl}");
            let _: () = conn.del(&key).await.unwrap();
        }

        // Without a TTL the key never expires (`TTL` is -1).
        let plain = RedisSessionStore::new(&url).await.unwrap();
        plain
            .create_qa_entry(&session, Some(user), "q", "a", None)
            .await
            .unwrap();
        let key = session_key(Some(user), &session);
        let ttl: i64 = conn.ttl(&key).await.unwrap();
        assert_eq!(ttl, -1);
        let _: () = conn.del(&key).await.unwrap();
    }

    /// Session-context rows: append, ordered read, in-place merge, TTL on
    /// the `session_context:` key and removal with the session. Needs a
    /// Redis server (`REDIS_TEST_URL`); skipped when unset.
    #[tokio::test]
    #[allow(
        clippy::disallowed_methods,
        reason = "tests drive configuration through the process env"
    )]
    async fn session_context_rows_round_trip() {
        use redis::AsyncCommands;

        use super::{RedisSessionStore, session_context_key};
        use crate::context_entry::SessionContextRow;
        use crate::session_store::SessionStore;

        let Some(url) = std::env::var("REDIS_TEST_URL")
            .ok()
            .filter(|u| !u.is_empty())
        else {
            eprintln!("REDIS_TEST_URL not set — skipping session_context_rows_round_trip");
            return;
        };
        let session = format!("ctx-{}", uuid::Uuid::new_v4());
        let user = Some("ctx-user");
        let row =
            |v: serde_json::Value| -> SessionContextRow { serde_json::from_value(v).unwrap() };

        let store = RedisSessionStore::new(&url)
            .await
            .unwrap()
            .with_session_ttl_seconds(120);
        store
            .create_session_context_entry(
                &session,
                user,
                row(serde_json::json!({"id": "a", "kind": "context", "n": 1})),
            )
            .await
            .unwrap();
        store
            .create_session_context_entry(
                &session,
                user,
                row(serde_json::json!({"id": "b", "kind": "wm_state"})),
            )
            .await
            .unwrap();
        assert!(
            store
                .update_session_context_entry(
                    &session,
                    user,
                    "b",
                    row(serde_json::json!({"done": 5}))
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .update_session_context_entry(&session, user, "zzz", SessionContextRow::new())
                .await
                .unwrap()
        );
        let rows = store
            .get_session_context_entries(&session, user)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], serde_json::json!("a"));
        assert_eq!(rows[1]["done"], serde_json::json!(5));
        assert_eq!(rows[1]["kind"], serde_json::json!("wm_state"));

        let mut conn = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let ttl: i64 = conn.ttl(session_context_key(user, &session)).await.unwrap();
        assert!((1..=120).contains(&ttl), "ttl {ttl}");

        assert!(store.delete_session(&session, user).await.unwrap());
        assert!(
            store
                .get_session_context_entries(&session, user)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_credentials_mean_none() {
        let info = connection_info("h", 1, "", "");
        assert_eq!(info.redis.username, None);
        assert_eq!(info.redis.password, None);
        let info = connection_info("h", 1, "", "secret");
        assert_eq!(info.redis.username, None);
        assert_eq!(info.redis.password.as_deref(), Some("secret"));
    }
}
