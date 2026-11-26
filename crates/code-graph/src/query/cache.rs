//! The process-wide cache of parsed code-graph snapshots.
//!
//! Port of Python's `_CodeGraphSnapshotCache` (`code_retriever.py:1042-1272`):
//! bounded (LRU, [`DEFAULT_CACHE_MAX_ENTRIES`] entries) and time-limited
//! ([`DEFAULT_CACHE_TTL`]), keyed by dataset and graph backend, with loads
//! that are single-flight per key and generation. Invalidation advances the
//! key's generation before it drops the entry, so a graph read that overlaps
//! a write can neither republish nor return its stale snapshot once that
//! write has finished.
//!
//! The code-graph writer invalidates the dataset it wrote after every node
//! and edge write ([`crate::add_code_graph_data_points`],
//! [`crate::add_code_graph_edges`]), as Python's pipeline does. Deleting or
//! forgetting a dataset's data invalidates it too
//! ([`invalidate_code_graph_snapshots`], called by `cognee-delete`): a
//! deliberate divergence, since Python invalidates on delete only when a code
//! repository is deleted from a dataset that survives
//! (`cluster.mark_dataset_code_repos_for_rebuild`) and otherwise serves the
//! pre-delete snapshot until the entry expires.
//!
//! Keys differ from Python's in two ways. The backend is the graph engine
//! object itself (Python hashes the graph configuration), held weakly, so an
//! entry never outlives its engine nor answers for a new engine that reuses
//! the old one's address. And invalidating a dataset also invalidates the
//! unscoped entries (searched outside any dataset scope, the shared store),
//! since the shared store holds every dataset's code.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use cognee_graph::GraphDBTrait;
use tokio::sync::OnceCell;
use uuid::Uuid;

use super::snapshot::CodeGraphSnapshot;
use crate::error::{CodeGraphError, CodeGraphResult};

/// Bumped when the snapshot's shape changes, so no entry of an older shape
/// is served (`_CODE_GRAPH_SNAPSHOT_FORMAT_VERSION`).
pub const SNAPSHOT_FORMAT_VERSION: u32 = 2;
/// Python's `_DEFAULT_CODE_GRAPH_CACHE_MAX_ENTRIES`.
pub const DEFAULT_CACHE_MAX_ENTRIES: usize = 16;
/// Python's `_DEFAULT_CODE_GRAPH_CACHE_TTL_SECONDS`.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60);

/// The bounds of a [`CodeGraphSnapshotCache`]
/// (`code_graph.snapshot_cache_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCacheLimits {
    /// The most snapshots kept; the least recently used goes first. At least
    /// one.
    pub max_entries: usize,
    /// How long a snapshot is served after its load. Positive.
    pub ttl: Duration,
}

impl Default for SnapshotCacheLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_CACHE_MAX_ENTRIES,
            ttl: DEFAULT_CACHE_TTL,
        }
    }
}

/// The identity of one cached snapshot: the format version, the dataset
/// whose scope it was read in (empty outside every scope) and the graph
/// engine it was read from. Holds no credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SnapshotCacheKey {
    format_version: u32,
    dataset_id: String,
    engine: usize,
}

impl SnapshotCacheKey {
    /// The key of `graph`'s snapshot in `dataset_id`'s scope.
    pub fn new(dataset_id: Option<Uuid>, graph: &Arc<dyn GraphDBTrait>) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            dataset_id: dataset_id.map(|id| id.to_string()).unwrap_or_default(),
            engine: Arc::as_ptr(graph).cast::<()>() as usize,
        }
    }

    /// The dataset part of the key: the id, or empty for an unscoped read.
    pub fn dataset_id(&self) -> &str {
        &self.dataset_id
    }
}

type Flight = (SnapshotCacheKey, u64);
type LoadOutcome = Result<Arc<CodeGraphSnapshot>, String>;

struct Entry {
    key: SnapshotCacheKey,
    snapshot: Arc<CodeGraphSnapshot>,
    engine: Weak<dyn GraphDBTrait>,
    expires_at: Instant,
}

#[derive(Default)]
struct CacheState {
    limits: SnapshotCacheLimits,
    /// Least recently used first.
    entries: Vec<Entry>,
    epochs: HashMap<SnapshotCacheKey, u64>,
    inflight: HashMap<Flight, Arc<OnceCell<LoadOutcome>>>,
    waiters: HashMap<Flight, usize>,
}

impl CacheState {
    fn position(&self, key: &SnapshotCacheKey) -> Option<usize> {
        self.entries.iter().position(|entry| &entry.key == key)
    }

    fn epoch(&self, key: &SnapshotCacheKey) -> u64 {
        self.epochs.get(key).copied().unwrap_or(0)
    }

    fn evict_lru(&mut self) {
        while self.entries.len() > self.limits.max_entries {
            let evicted = self.entries.remove(0);
            self.prune_epoch_if_idle(&evicted.key);
        }
    }

    /// Forget a key's generation once nothing refers to it: no entry, no
    /// load in flight and no caller waiting on one.
    fn prune_epoch_if_idle(&mut self, key: &SnapshotCacheKey) {
        if self.position(key).is_some()
            || self.inflight.keys().any(|(flight, _)| flight == key)
            || self.waiters.keys().any(|(flight, _)| flight == key)
        {
            return;
        }
        self.epochs.remove(key);
    }

    fn invalidate(&mut self, key: &SnapshotCacheKey) {
        if let Some(position) = self.position(key) {
            self.entries.remove(position);
        }
        *self.epochs.entry(key.clone()).or_insert(0) += 1;
        self.prune_epoch_if_idle(key);
    }

    fn known_keys(&self) -> Vec<SnapshotCacheKey> {
        let mut keys: Vec<SnapshotCacheKey> =
            self.entries.iter().map(|entry| entry.key.clone()).collect();
        keys.extend(self.epochs.keys().cloned());
        keys.extend(self.inflight.keys().map(|(key, _)| key.clone()));
        keys.extend(self.waiters.keys().map(|(key, _)| key.clone()));
        keys.sort_by(|left, right| {
            (&left.dataset_id, left.engine, left.format_version).cmp(&(
                &right.dataset_id,
                right.engine,
                right.format_version,
            ))
        });
        keys.dedup();
        keys
    }
}

/// A bounded, concurrency-safe cache of parsed code-graph snapshots
/// (`_CodeGraphSnapshotCache`). See the `query::cache` module docs.
pub struct CodeGraphSnapshotCache {
    state: Mutex<CacheState>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl std::fmt::Debug for CodeGraphSnapshotCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeGraphSnapshotCache")
            .field("limits", &self.limits())
            .field("len", &self.len())
            .finish()
    }
}

impl CodeGraphSnapshotCache {
    /// An empty cache with `limits`.
    pub fn new(limits: SnapshotCacheLimits) -> Self {
        Self::with_clock(limits, Arc::new(Instant::now))
    }

    /// An empty cache that reads the time from `clock` (for tests of the
    /// time limit).
    pub fn with_clock(
        limits: SnapshotCacheLimits,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            state: Mutex::new(CacheState {
                limits: sanitize(limits),
                ..CacheState::default()
            }),
            clock,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            // lock poison is unrecoverable for the panicking thread only: the
            // state holds no invariant a panic can break half way, so keep
            // serving.
            poisoned.into_inner()
        })
    }

    /// The cache's bounds.
    pub fn limits(&self) -> SnapshotCacheLimits {
        self.state().limits
    }

    /// Change the cache's bounds; entries beyond a smaller `max_entries` are
    /// evicted, least recently used first. A new time limit applies to
    /// snapshots loaded from now on.
    pub fn set_limits(&self, limits: SnapshotCacheLimits) {
        let mut state = self.state();
        state.limits = sanitize(limits);
        state.evict_lru();
    }

    /// The number of snapshots held (expired ones included until they are
    /// next looked up).
    pub fn len(&self) -> usize {
        self.state().entries.len()
    }

    /// Whether no snapshot is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a live, unexpired snapshot is held for `key`.
    pub fn contains(&self, key: &SnapshotCacheKey) -> bool {
        let now = (self.clock)();
        let state = self.state();
        state.position(key).is_some_and(|position| {
            let entry = &state.entries[position];
            entry.expires_at > now && entry.engine.strong_count() > 0
        })
    }

    /// The snapshot of `graph` in `dataset_id`'s scope: the cached one while
    /// it is fresh, else one `loader` reads. Concurrent callers for the same
    /// key share one load; a caller whose load overlapped an invalidation
    /// loads again rather than return what it read.
    ///
    /// # Errors
    ///
    /// The load failed ([`CodeGraphError::SnapshotLoad`]). Every caller that
    /// shared the failed load gets the error, as in Python.
    pub async fn get_or_load<F, Fut>(
        &self,
        dataset_id: Option<Uuid>,
        graph: &Arc<dyn GraphDBTrait>,
        loader: F,
    ) -> CodeGraphResult<Arc<CodeGraphSnapshot>>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = CodeGraphResult<CodeGraphSnapshot>>,
    {
        let key = SnapshotCacheKey::new(dataset_id, graph);
        loop {
            let (cell, flight) = {
                let now = (self.clock)();
                let mut state = self.state();
                if let Some(position) = state.position(&key) {
                    let entry = &state.entries[position];
                    if entry.expires_at > now && entry.engine.strong_count() > 0 {
                        let entry = state.entries.remove(position);
                        let snapshot = Arc::clone(&entry.snapshot);
                        state.entries.push(entry);
                        return Ok(snapshot);
                    }
                    state.entries.remove(position);
                }
                let epoch = *state.epochs.entry(key.clone()).or_insert(0);
                let flight = (key.clone(), epoch);
                let cell = Arc::clone(
                    state
                        .inflight
                        .entry(flight.clone())
                        .or_insert_with(|| Arc::new(OnceCell::new())),
                );
                *state.waiters.entry(flight.clone()).or_insert(0) += 1;
                (cell, flight)
            };
            let waiter = Waiter {
                cache: self,
                flight: flight.clone(),
                cell: Arc::clone(&cell),
            };

            let outcome = cell
                .get_or_init(|| async {
                    loader()
                        .await
                        .map(Arc::new)
                        .map_err(|error| error.to_string())
                })
                .await
                .clone();

            let still_current = {
                let mut state = self.state();
                // The first caller to see the finished load publishes it.
                let first = state
                    .inflight
                    .get(&flight)
                    .is_some_and(|stored| Arc::ptr_eq(stored, &cell));
                if first {
                    state.inflight.remove(&flight);
                    if let Ok(snapshot) = &outcome
                        && state.epoch(&key) == flight.1
                    {
                        let ttl = state.limits.ttl;
                        if let Some(position) = state.position(&key) {
                            state.entries.remove(position);
                        }
                        state.entries.push(Entry {
                            key: key.clone(),
                            snapshot: Arc::clone(snapshot),
                            engine: Arc::downgrade(graph),
                            expires_at: (self.clock)() + ttl,
                        });
                        state.evict_lru();
                    }
                }
                state.epoch(&key) == flight.1
            };
            drop(waiter);

            match outcome {
                Ok(snapshot) if still_current => return Ok(snapshot),
                Err(message) if still_current => return Err(CodeGraphError::SnapshotLoad(message)),
                // Invalidated while loading: read again.
                _ => continue,
            }
        }
    }

    /// Invalidate every snapshot read in `dataset_id`'s scope, and every
    /// unscoped one: the data of the dataset changed.
    pub fn invalidate_dataset(&self, dataset_id: Uuid) {
        let dataset = dataset_id.to_string();
        let mut state = self.state();
        for key in state.known_keys() {
            if key.dataset_id == dataset || key.dataset_id.is_empty() {
                state.invalidate(&key);
            }
        }
    }

    /// Invalidate every snapshot, without cancelling loads in flight (their
    /// callers read again).
    pub fn clear(&self) {
        let mut state = self.state();
        let keys = state.known_keys();
        state.entries.clear();
        for key in &keys {
            *state.epochs.entry(key.clone()).or_insert(0) += 1;
        }
        for key in &keys {
            state.prune_epoch_if_idle(key);
        }
    }
}

fn sanitize(limits: SnapshotCacheLimits) -> SnapshotCacheLimits {
    SnapshotCacheLimits {
        max_entries: limits.max_entries.max(1),
        ttl: if limits.ttl.is_zero() {
            DEFAULT_CACHE_TTL
        } else {
            limits.ttl
        },
    }
}

/// One caller waiting on a load: counted while it waits, so the key's
/// generation is not forgotten under it (`_remove_waiter`).
struct Waiter<'a> {
    cache: &'a CodeGraphSnapshotCache,
    flight: Flight,
    cell: Arc<OnceCell<LoadOutcome>>,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut state = self.cache.state();
        let remaining = state
            .waiters
            .get(&self.flight)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if remaining > 0 {
            state.waiters.insert(self.flight.clone(), remaining);
        } else {
            state.waiters.remove(&self.flight);
            // Every caller of an unfinished load was cancelled: drop it, so
            // the next caller starts afresh instead of finding a stale cell.
            if !self.cell.initialized()
                && state
                    .inflight
                    .get(&self.flight)
                    .is_some_and(|stored| Arc::ptr_eq(stored, &self.cell))
            {
                state.inflight.remove(&self.flight);
            }
        }
        state.prune_epoch_if_idle(&self.flight.0);
    }
}

/// The process-wide snapshot cache every CODE search reads through, with the
/// default bounds until [`configure_code_graph_snapshot_cache`] sets them.
pub fn code_graph_snapshot_cache() -> &'static CodeGraphSnapshotCache {
    static CACHE: OnceLock<CodeGraphSnapshotCache> = OnceLock::new();
    CACHE.get_or_init(|| CodeGraphSnapshotCache::new(SnapshotCacheLimits::default()))
}

/// Apply the configured bounds (`code_graph.snapshot_cache_*`) to the
/// process-wide cache.
pub fn configure_code_graph_snapshot_cache(limits: SnapshotCacheLimits) {
    code_graph_snapshot_cache().set_limits(limits);
}

/// Invalidate the parsed CODE snapshots after a graph mutation of
/// `dataset_id` (Python's `invalidate_code_graph_snapshot_cache`): that
/// dataset's snapshots and the unscoped ones. Without a dataset, the current
/// dataset scope ([`cognee_models::dataset_scope::current_dataset`]) is
/// used; outside every scope every snapshot is invalidated, since the
/// mutated dataset is unknown.
pub fn invalidate_code_graph_snapshots(dataset_id: Option<Uuid>) {
    match dataset_id.or_else(cognee_models::dataset_scope::current_dataset) {
        Some(dataset_id) => code_graph_snapshot_cache().invalidate_dataset(dataset_id),
        None => code_graph_snapshot_cache().clear(),
    }
}

/// Invalidate every parsed CODE snapshot (Python's
/// `invalidate_code_graph_snapshot_cache(all_entries=True)`).
pub fn clear_code_graph_snapshot_cache() {
    code_graph_snapshot_cache().clear();
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use cognee_graph::MockGraphDB;

    use super::*;

    struct ManualClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            })
        }

        fn advance(&self, duration: Duration) {
            self.offset_ms
                .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        }

        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    fn cache(clock: &Arc<ManualClock>, max_entries: usize) -> CodeGraphSnapshotCache {
        let clock = Arc::clone(clock);
        CodeGraphSnapshotCache::with_clock(
            SnapshotCacheLimits {
                max_entries,
                ttl: Duration::from_secs(60),
            },
            Arc::new(move || clock.now()),
        )
    }

    fn graph() -> Arc<dyn GraphDBTrait> {
        Arc::new(MockGraphDB::new())
    }

    async fn load(
        cache: &CodeGraphSnapshotCache,
        dataset: Option<Uuid>,
        graph: &Arc<dyn GraphDBTrait>,
        loads: &AtomicUsize,
    ) -> Arc<CodeGraphSnapshot> {
        cache
            .get_or_load(dataset, graph, || async {
                loads.fetch_add(1, Ordering::SeqCst);
                Ok(CodeGraphSnapshot::empty())
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_fresh_snapshot_is_served_until_its_ttl() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let graph = graph();
        let loads = AtomicUsize::new(0);
        let first = load(&cache, None, &graph, &loads).await;
        let again = load(&cache, None, &graph, &loads).await;
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        clock.advance(Duration::from_secs(59));
        load(&cache, None, &graph, &loads).await;
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        clock.advance(Duration::from_secs(1));
        assert!(!cache.contains(&SnapshotCacheKey::new(None, &graph)));
        load(&cache, None, &graph, &loads).await;
        assert_eq!(loads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_least_recently_used_snapshot_is_evicted() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 2);
        let graph = graph();
        let loads = AtomicUsize::new(0);
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load(&cache, Some(a), &graph, &loads).await;
        load(&cache, Some(b), &graph, &loads).await;
        // Touch a: b becomes the least recently used.
        load(&cache, Some(a), &graph, &loads).await;
        load(&cache, Some(c), &graph, &loads).await;
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&SnapshotCacheKey::new(Some(a), &graph)));
        assert!(!cache.contains(&SnapshotCacheKey::new(Some(b), &graph)));
        assert!(cache.contains(&SnapshotCacheKey::new(Some(c), &graph)));
        assert_eq!(loads.load(Ordering::SeqCst), 3);

        cache.set_limits(SnapshotCacheLimits {
            max_entries: 1,
            ttl: Duration::from_secs(60),
        });
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&SnapshotCacheKey::new(Some(c), &graph)));
    }

    #[tokio::test]
    async fn invalidating_a_dataset_drops_it_and_the_unscoped_snapshots_only() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let graph = graph();
        let loads = AtomicUsize::new(0);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        load(&cache, Some(a), &graph, &loads).await;
        load(&cache, Some(b), &graph, &loads).await;
        load(&cache, None, &graph, &loads).await;
        cache.invalidate_dataset(a);
        assert!(!cache.contains(&SnapshotCacheKey::new(Some(a), &graph)));
        assert!(cache.contains(&SnapshotCacheKey::new(Some(b), &graph)));
        assert!(!cache.contains(&SnapshotCacheKey::new(None, &graph)));
        cache.clear();
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn keys_separate_engines_and_a_dropped_engine_is_never_served() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let loads = AtomicUsize::new(0);
        let first = graph();
        let second = graph();
        load(&cache, None, &first, &loads).await;
        load(&cache, None, &second, &loads).await;
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        let key = SnapshotCacheKey::new(None, &first);
        drop(first);
        assert!(!cache.contains(&key));
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_load() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let graph = graph();
        let loads = AtomicUsize::new(0);
        let gate = tokio::sync::Notify::new();
        let loader = || async {
            loads.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
            Ok(CodeGraphSnapshot::empty())
        };
        let first = cache.get_or_load(None, &graph, loader);
        let second = cache.get_or_load(None, &graph, loader);
        let release = async {
            tokio::task::yield_now().await;
            gate.notify_waiters();
            tokio::task::yield_now().await;
            gate.notify_waiters();
        };
        let (first, second, ()) = tokio::join!(first, second, release);
        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_load_overlapping_an_invalidation_is_not_served() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let graph = graph();
        let loads = AtomicUsize::new(0);
        let dataset = Uuid::new_v4();
        let snapshot = cache
            .get_or_load(Some(dataset), &graph, || async {
                // A write finishes while the first read is in flight.
                if loads.fetch_add(1, Ordering::SeqCst) == 0 {
                    cache.invalidate_dataset(dataset);
                }
                Ok(CodeGraphSnapshot::empty())
            })
            .await
            .unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert!(cache.contains(&SnapshotCacheKey::new(Some(dataset), &graph)));
        drop(snapshot);
    }

    #[tokio::test]
    async fn a_failed_load_is_reported_and_not_cached() {
        let clock = ManualClock::new();
        let cache = cache(&clock, 16);
        let graph = graph();
        let error = cache
            .get_or_load(None, &graph, || async {
                Err(CodeGraphError::InvalidConfig("boom".into()))
            })
            .await
            .unwrap_err();
        assert!(matches!(error, CodeGraphError::SnapshotLoad(message) if message == "boom"));
        assert!(cache.is_empty());
        let loads = AtomicUsize::new(0);
        load(&cache, None, &graph, &loads).await;
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
