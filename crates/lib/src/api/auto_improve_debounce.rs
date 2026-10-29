//! Debounce of the automatic session-to-graph improve — Python's
//! `cognee/api/v1/remember/auto_improve_debounce.py`.
//!
//! A session `remember()` bridges the session into the permanent graph by
//! launching `improve(session_ids=[session_id])` after every call. With
//! `improve.debounce_entries` / `improve.debounce_seconds` the bridge fires
//! only when enough new session entries accumulated since the last automatic
//! improve, **or** enough time passed since it.
//!
//! The per-session bookkeeping (the Q&A count and when the last automatic
//! improve fired) is an internal session-context row,
//! `{"id": "auto_improve_debounce", "kind": "auto_improve_debounce_state",
//! "state": {"qa_count", "last_improve_at"}}`, written through
//! [`StateRowWatermark`], so clearing the session clears it too.
//!
//! * `debounce_entries = N` fires when at least `N` entries were added since
//!   the last automatic improve; `0` turns the entry trigger off.
//! * `debounce_seconds = T` fires when at least `T` seconds elapsed; `0`
//!   turns the time trigger off.
//! * A seconds-only configuration is time-only: the entries default of `1`
//!   would fire on every call, so with `debounce_seconds > 0` it steps aside
//!   (set `>= 2` to combine both triggers).
//! * The defaults (`1` entry, `0` seconds) mean no debounce: every call
//!   bridges and nothing is read from the session store.
//! * A session with no state row yet fires (first run); a failed read fires
//!   too (fail-open).
//! * There is no timer: entries below the thresholds wait for the next call.

use chrono::{DateTime, Utc};
use cognee_session::{SessionManager, StateRowWatermark};
use serde_json::{Value, json};
use tracing::debug;

use super::improve::ImproveConfig;

/// Row id of the debounce state.
pub const AUTO_IMPROVE_STATE_ID: &str = "auto_improve_debounce";
/// Row kind of the debounce state; never `"context"`, so it is never rendered.
pub const AUTO_IMPROVE_STATE_KIND: &str = "auto_improve_debounce_state";

/// One state row per (user, session), holding `{"qa_count", "last_improve_at"}`.
pub const AUTO_IMPROVE_WATERMARK: StateRowWatermark =
    StateRowWatermark::new(AUTO_IMPROVE_STATE_ID, AUTO_IMPROVE_STATE_KIND, "state");

/// Why the automatic improve does or does not fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoImproveReason {
    /// The thresholds can never hold an improve back.
    NoDebounce,
    /// No state row yet.
    FirstRun,
    /// Enough new entries.
    Entries,
    /// Enough time elapsed.
    Elapsed,
    /// Below both thresholds.
    Debounced,
    /// The state could not be read; fail-open.
    StateUnavailable,
}

impl AutoImproveReason {
    /// Python's reason string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoDebounce => "no_debounce",
            Self::FirstRun => "first_run",
            Self::Entries => "entries",
            Self::Elapsed => "elapsed",
            Self::Debounced => "debounced",
            Self::StateUnavailable => "state_unavailable",
        }
    }
}

/// Whether the automatic improve should fire for this `remember()` call.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoImproveDecision {
    pub due: bool,
    pub reason: AutoImproveReason,
    /// Q&A entries in the session now (0 when not read).
    pub qa_count: u64,
    /// Entries added since the last automatic improve.
    pub new_entries: u64,
    /// Seconds since the last automatic improve, when known.
    pub elapsed_seconds: Option<f64>,
}

impl AutoImproveDecision {
    fn fire(reason: AutoImproveReason) -> Self {
        Self {
            due: true,
            reason,
            qa_count: 0,
            new_entries: 0,
            elapsed_seconds: None,
        }
    }
}

/// The effective thresholds: a seconds-only configuration is time-only.
fn thresholds(config: &ImproveConfig) -> (u64, f64) {
    let seconds = config.debounce_seconds.max(0.0);
    let entries = if seconds > 0.0 && config.debounce_entries == 1 {
        0
    } else {
        config.debounce_entries
    };
    (entries, seconds)
}

/// True when the configured thresholds can ever hold an improve back.
pub fn debounce_active(config: &ImproveConfig) -> bool {
    config.debounce_entries > 1 || config.debounce_seconds > 0.0
}

/// A lenient count: integers, truncated floats and integer strings; anything
/// else (or a negative) is 0, as Python's `max(0, int(value or 0))`.
fn lenient_count(value: Option<&Value>) -> u64 {
    let n = match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    };
    u64::try_from(n).unwrap_or(0)
}

fn parse_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let raw = value?.as_str()?;
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Some(parsed.with_timezone(&Utc));
    }
    // A naive timestamp is UTC, as Python assumes.
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|naive| naive.and_utc())
}

async fn count_qa_entries(
    manager: &SessionManager,
    user_id: &str,
    session_id: &str,
) -> Result<u64, cognee_session::SessionError> {
    let entries = manager.get_session(Some(session_id), Some(user_id)).await?;
    Ok(u64::try_from(entries.len()).unwrap_or(u64::MAX))
}

/// Decide whether this `remember()` call should launch the improve bridge.
/// Called after the new entry was written, so the count includes it.
pub async fn should_auto_improve(
    manager: &SessionManager,
    config: &ImproveConfig,
    user_id: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> AutoImproveDecision {
    let (entries_threshold, seconds_threshold) = thresholds(config);
    if entries_threshold <= 1 && seconds_threshold <= 0.0 {
        return AutoImproveDecision::fire(AutoImproveReason::NoDebounce);
    }

    let read = async {
        let state = AUTO_IMPROVE_WATERMARK
            .read_value(manager, Some(session_id), Some(user_id))
            .await?;
        let qa_count = count_qa_entries(manager, user_id, session_id).await?;
        Ok::<_, cognee_session::SessionError>((state, qa_count))
    };
    let (state, qa_count) = match read.await {
        Ok(read) => read,
        Err(error) => {
            debug!(%error, "auto-improve debounce: state unavailable, firing");
            return AutoImproveDecision::fire(AutoImproveReason::StateUnavailable);
        }
    };

    // No row yet, or a malformed value: fire.
    let Some(Value::Object(state)) = state else {
        return AutoImproveDecision {
            qa_count,
            ..AutoImproveDecision::fire(AutoImproveReason::FirstRun)
        };
    };

    let last_count = lenient_count(state.get("qa_count"));
    // A watermark above the current count means the session was cleared and
    // rebuilt; every current entry is new then.
    let new_entries = if qa_count < last_count {
        qa_count
    } else {
        qa_count - last_count
    };
    let elapsed_seconds = parse_timestamp(state.get("last_improve_at"))
        .map(|last| (now - last).num_milliseconds() as f64 / 1000.0);

    let reason = if entries_threshold > 0 && new_entries >= entries_threshold {
        AutoImproveReason::Entries
    } else if seconds_threshold > 0.0 && elapsed_seconds.is_none_or(|e| e >= seconds_threshold) {
        AutoImproveReason::Elapsed
    } else {
        AutoImproveReason::Debounced
    };
    AutoImproveDecision {
        due: reason != AutoImproveReason::Debounced,
        reason,
        qa_count,
        new_entries,
        elapsed_seconds,
    }
}

/// Record that an automatic improve was launched for this session.
///
/// Written before the improve runs, so back-to-back `remember()` calls see
/// the advanced watermark; an improve that errors keeps the mark. Never
/// fails: losing the row only means the next call fires one improve early.
/// `qa_count` of `None` (or 0) counts the session's entries.
pub async fn mark_auto_improve_fired(
    manager: &SessionManager,
    user_id: &str,
    session_id: &str,
    qa_count: Option<u64>,
    now: DateTime<Utc>,
) {
    let write = async {
        let qa_count = match qa_count.filter(|n| *n > 0) {
            Some(n) => n,
            None => count_qa_entries(manager, user_id, session_id).await?,
        };
        AUTO_IMPROVE_WATERMARK
            .write_value(
                manager,
                Some(session_id),
                Some(user_id),
                json!({ "qa_count": qa_count, "last_improve_at": now.to_rfc3339() }),
            )
            .await
    };
    if let Err(error) = write.await {
        debug!(%error, "auto-improve debounce: could not save state");
    }
}

/// Apply the debounce and advance its state when firing — Python's
/// `_session_improve_due`. No debounce configured, or no usable session
/// store, fires without reading anything.
pub async fn session_improve_due(
    manager: Option<&SessionManager>,
    config: &ImproveConfig,
    user_id: &str,
    session_id: &str,
) -> bool {
    if !debounce_active(config) {
        return true;
    }
    let Some(manager) = manager.filter(|m| m.is_available()) else {
        return true;
    };
    let now = Utc::now();
    let decision = should_auto_improve(manager, config, user_id, session_id, now).await;
    if !decision.due {
        debug!(
            new_entries = decision.new_entries,
            elapsed_seconds = ?decision.elapsed_seconds,
            "remember: session improve debounced"
        );
        return false;
    }
    mark_auto_improve_fired(manager, user_id, session_id, Some(decision.qa_count), now).await;
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    //! Ported from Python's
    //! `cognee/tests/unit/api/v1/remember/test_remember_auto_improve_debounce.py`
    //! (the decision and state-row cases; the `remember()` cases are in
    //! `crates/lib/tests/remember_tests.rs`).

    use std::sync::Arc;

    use chrono::TimeZone;
    use cognee_session::{FsSessionStore, SessionContextRow, SessionStore};

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Arc<dyn SessionStore>,
        manager: SessionManager,
        user: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(dir.path().to_path_buf()));
        let manager = SessionManager::new(Arc::clone(&store));
        Fixture {
            _dir: dir,
            store,
            manager,
            user: uuid::Uuid::new_v4().to_string(),
        }
    }

    impl Fixture {
        async fn add_entries(&self, session: &str, n: usize) {
            for i in 0..n {
                self.store
                    .create_qa_entry(session, Some(&self.user), "", &format!("turn {i}"), None)
                    .await
                    .unwrap();
            }
        }

        async fn decide(
            &self,
            config: &ImproveConfig,
            session: &str,
            now: DateTime<Utc>,
        ) -> AutoImproveDecision {
            should_auto_improve(&self.manager, config, &self.user, session, now).await
        }

        async fn rows(&self, session: &str) -> Vec<SessionContextRow> {
            self.manager
                .get_session_context_entries(Some(session), Some(&self.user))
                .await
                .unwrap()
        }
    }

    fn config(entries: u64, seconds: f64) -> ImproveConfig {
        ImproveConfig {
            debounce_entries: entries,
            debounce_seconds: seconds,
            ..ImproveConfig::default()
        }
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 12, 0, 0).unwrap()
    }

    // test_debounce_fires_at_most_ceil_n_over_entries, at the decision level.
    #[tokio::test]
    async fn debounce_fires_every_n_entries() {
        for (n, entries) in [(7u64, 3u64), (10, 5), (4, 4), (5, 1)] {
            let f = fixture();
            let config = config(entries, 0.0);
            let mut fired = Vec::new();
            for _ in 0..n {
                f.add_entries("s", 1).await;
                fired.push(session_improve_due(Some(&f.manager), &config, &f.user, "s").await);
            }
            let expected: Vec<bool> = (0..n).map(|i| i % entries == 0).collect();
            assert_eq!(fired, expected, "n={n} entries={entries}");
            assert_eq!(
                fired.iter().filter(|f| **f).count() as u64,
                n.div_ceil(entries)
            );
        }
    }

    // test_default_config_fires_every_time_and_reads_nothing
    #[tokio::test]
    async fn default_config_fires_every_time_and_writes_nothing() {
        let f = fixture();
        let config = ImproveConfig::default();
        assert!(!debounce_active(&config));
        for _ in 0..4 {
            f.add_entries("s", 1).await;
            assert!(session_improve_due(Some(&f.manager), &config, &f.user, "s").await);
        }
        assert!(f.rows("s").await.is_empty());
        let decision = f.decide(&config, "s", t0()).await;
        assert_eq!(decision.reason, AutoImproveReason::NoDebounce);
    }

    // test_time_trigger_fires_after_debounce_seconds
    #[tokio::test]
    async fn time_trigger_fires_after_debounce_seconds() {
        let f = fixture();
        let config = config(0, 60.0);
        f.add_entries("s-time", 1).await;
        let first = f.decide(&config, "s-time", t0()).await;
        assert!(first.due);
        assert_eq!(first.reason, AutoImproveReason::FirstRun);
        mark_auto_improve_fired(&f.manager, &f.user, "s-time", Some(first.qa_count), t0()).await;
        f.add_entries("s-time", 3).await;

        let soon = f
            .decide(&config, "s-time", t0() + chrono::Duration::seconds(30))
            .await;
        assert!(!soon.due);
        assert_eq!(soon.reason, AutoImproveReason::Debounced);
        assert_eq!(soon.new_entries, 3);

        let later = f
            .decide(&config, "s-time", t0() + chrono::Duration::seconds(61))
            .await;
        assert!(later.due);
        assert_eq!(later.reason, AutoImproveReason::Elapsed);
    }

    // test_seconds_only_config_debounces_despite_the_entries_default
    #[tokio::test]
    async fn seconds_only_config_debounces_despite_the_entries_default() {
        let f = fixture();
        let config = config(1, 60.0);
        f.add_entries("s", 1).await;
        let first = f.decide(&config, "s", t0()).await;
        assert_eq!(first.reason, AutoImproveReason::FirstRun);
        mark_auto_improve_fired(&f.manager, &f.user, "s", Some(first.qa_count), t0()).await;

        f.add_entries("s", 1).await;
        let soon = f
            .decide(&config, "s", t0() + chrono::Duration::seconds(30))
            .await;
        assert!(!soon.due);
        assert_eq!(soon.reason, AutoImproveReason::Debounced);
        let later = f
            .decide(&config, "s", t0() + chrono::Duration::seconds(61))
            .await;
        assert_eq!(later.reason, AutoImproveReason::Elapsed);
    }

    // test_explicitly_raised_entries_combine_with_seconds
    #[tokio::test]
    async fn explicitly_raised_entries_combine_with_seconds() {
        let f = fixture();
        let config = config(2, 3600.0);
        f.add_entries("s", 1).await;
        let first = f.decide(&config, "s", t0()).await;
        assert!(first.due);
        mark_auto_improve_fired(&f.manager, &f.user, "s", Some(first.qa_count), t0()).await;

        f.add_entries("s", 1).await;
        let one_new = f
            .decide(&config, "s", t0() + chrono::Duration::seconds(1))
            .await;
        assert!(!one_new.due);
        f.add_entries("s", 1).await;
        let two_new = f
            .decide(&config, "s", t0() + chrono::Duration::seconds(2))
            .await;
        assert!(two_new.due);
        assert_eq!(two_new.reason, AutoImproveReason::Entries);
    }

    // test_state_row_uses_internal_context_row_pattern
    #[tokio::test]
    async fn state_row_uses_internal_context_row_pattern() {
        let f = fixture();
        f.add_entries("s-row", 2).await;
        mark_auto_improve_fired(&f.manager, &f.user, "s-row", None, t0()).await;
        let rows = f.rows("s-row").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], AUTO_IMPROVE_STATE_ID);
        assert_eq!(rows[0]["kind"], AUTO_IMPROVE_STATE_KIND);
        assert_eq!(rows[0]["state"]["qa_count"], 2);
        let at = rows[0]["state"]["last_improve_at"].as_str().unwrap();
        assert!(DateTime::parse_from_rfc3339(at).is_ok(), "{at}");

        // A second fire merges into the same row instead of appending another.
        f.add_entries("s-row", 1).await;
        mark_auto_improve_fired(&f.manager, &f.user, "s-row", None, t0()).await;
        let rows = f.rows("s-row").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"]["qa_count"], 3);
    }

    // test_cleared_session_counts_all_entries_as_new
    #[tokio::test]
    async fn cleared_session_counts_all_entries_as_new() {
        let f = fixture();
        AUTO_IMPROVE_WATERMARK
            .write_value(
                &f.manager,
                Some("s-cleared"),
                Some(&f.user),
                json!({ "qa_count": 10, "last_improve_at": Utc::now().to_rfc3339() }),
            )
            .await
            .unwrap();
        f.add_entries("s-cleared", 3).await;
        let decision = f.decide(&config(3, 0.0), "s-cleared", Utc::now()).await;
        assert!(decision.due);
        assert_eq!(decision.reason, AutoImproveReason::Entries);
        assert_eq!(decision.new_entries, 3);
    }

    // test_state_read_failure_fails_open: a store without context-row support
    // fails the read.
    #[tokio::test]
    async fn state_read_failure_fails_open() {
        let f = fixture();
        let broken = SessionManager::new(Arc::new(NoContextRows(Arc::clone(&f.store))));
        let decision =
            should_auto_improve(&broken, &config(5, 0.0), &f.user, "s", Utc::now()).await;
        assert!(decision.due);
        assert_eq!(decision.reason, AutoImproveReason::StateUnavailable);
    }

    #[tokio::test]
    async fn unavailable_session_store_fires_without_reading() {
        let manager = SessionManager::new(Arc::new(cognee_session::DisabledSessionStore));
        assert!(session_improve_due(Some(&manager), &config(5, 0.0), "u", "s").await);
        assert!(session_improve_due(None, &config(5, 0.0), "u", "s").await);
    }

    #[test]
    fn timestamps_parse_with_or_without_a_zone() {
        assert_eq!(
            parse_timestamp(Some(&json!("2026-09-04T12:00:00+00:00"))),
            Some(t0())
        );
        assert_eq!(
            parse_timestamp(Some(&json!("2026-09-04T12:00:00"))),
            Some(t0())
        );
        assert_eq!(parse_timestamp(Some(&json!("not a date"))), None);
        assert_eq!(parse_timestamp(None), None);
    }

    /// Delegates Q&A storage, keeps the trait's default (failing)
    /// context-row methods.
    struct NoContextRows(Arc<dyn SessionStore>);

    #[async_trait::async_trait]
    impl SessionStore for NoContextRows {
        async fn create_qa_entry(
            &self,
            session_id: &str,
            user_id: Option<&str>,
            question: &str,
            answer: &str,
            context: Option<&str>,
        ) -> Result<String, cognee_session::SessionError> {
            self.0
                .create_qa_entry(session_id, user_id, question, answer, context)
                .await
        }
        async fn get_latest_qa_entries(
            &self,
            session_id: &str,
            user_id: Option<&str>,
            last_n: usize,
        ) -> Result<Vec<cognee_session::SessionQAEntry>, cognee_session::SessionError> {
            self.0
                .get_latest_qa_entries(session_id, user_id, last_n)
                .await
        }
        async fn get_all_qa_entries(
            &self,
            session_id: &str,
            user_id: Option<&str>,
        ) -> Result<Vec<cognee_session::SessionQAEntry>, cognee_session::SessionError> {
            self.0.get_all_qa_entries(session_id, user_id).await
        }
        async fn delete_session(
            &self,
            session_id: &str,
            user_id: Option<&str>,
        ) -> Result<bool, cognee_session::SessionError> {
            self.0.delete_session(session_id, user_id).await
        }
        async fn delete_qa_entry(
            &self,
            session_id: &str,
            user_id: Option<&str>,
            qa_id: &str,
        ) -> Result<bool, cognee_session::SessionError> {
            self.0.delete_qa_entry(session_id, user_id, qa_id).await
        }
        async fn prune(&self) -> Result<(), cognee_session::SessionError> {
            self.0.prune().await
        }
        async fn update_qa_entry(
            &self,
            session_id: &str,
            user_id: Option<&str>,
            qa_id: &str,
            updates: cognee_session::SessionQAUpdate,
        ) -> Result<bool, cognee_session::SessionError> {
            self.0
                .update_qa_entry(session_id, user_id, qa_id, updates)
                .await
        }
    }
}
