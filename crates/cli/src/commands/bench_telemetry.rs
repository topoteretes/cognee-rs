//! Offline per-stage wall-clock telemetry for the `bench` subcommand.
//!
//! The pipeline is already covered with `#[tracing::instrument]` spans. This
//! layer reuses them instead of adding new instrumentation. It installs into
//! the CLI's global subscriber and, while armed, accumulates per-span busy time
//! (actively executing) and idle time (alive but awaiting). The bench driver
//! arms it around each phase and writes a breakdown.
//!
//! It complements the pprof flamegraphs. A sampling profiler only sees threads
//! that are on-CPU, so it cannot account for the await/IO time that dominates
//! the pipeline at small corpus sizes. The busy figure mirrors what the
//! flamegraph measures. The idle figure is the off-CPU time it misses. Together
//! they cover the full wall-clock of each stage.
//!
//! Aggregation is by span name across all instances. For parallel stages such
//! as concurrent chunk extractions the summed total can exceed real wall-clock
//! time, so treat it as a relative attribution of work per stage rather than an
//! exclusive timeline.
#![allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use cognee_logging::BoxedLayer;
use serde::Serialize;
use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Per-span-name aggregate, in nanoseconds.
#[derive(Default, Clone)]
struct SpanAgg {
    /// Number of spans of this name closed while armed.
    count: u64,
    /// Total time spans of this name were actively executing (on CPU).
    busy_ns: u128,
    /// Total time spans of this name were alive but awaiting (off-CPU).
    idle_ns: u128,
}

/// Shared registry the layer writes into and the bench driver reads.
struct Store {
    enabled: AtomicBool,
    aggs: Mutex<BTreeMap<&'static str, SpanAgg>>,
}

static STORE: OnceLock<Store> = OnceLock::new();

fn store() -> &'static Store {
    STORE.get_or_init(|| Store {
        enabled: AtomicBool::new(false),
        aggs: Mutex::new(BTreeMap::new()),
    })
}

/// Per-span timing state kept in the span's extensions.
struct Timings {
    busy: Duration,
    idle: Duration,
    /// Timestamp of the last state transition (open / enter / exit).
    last: Instant,
}

/// A `tracing` layer that accumulates busy/idle time per span name while armed.
pub struct SpanTimingLayer;

impl<S> Layer<S> for SpanTimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Timings {
                busy: Duration::ZERO,
                idle: Duration::ZERO,
                last: Instant::now(),
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(t) = span.extensions_mut().get_mut::<Timings>()
        {
            let now = Instant::now();
            t.idle += now.saturating_duration_since(t.last);
            t.last = now;
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(t) = span.extensions_mut().get_mut::<Timings>()
        {
            let now = Instant::now();
            t.busy += now.saturating_duration_since(t.last);
            t.last = now;
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let store = store();
        if !store.enabled.load(Ordering::Relaxed) {
            return;
        }
        let Some(span) = ctx.span(&id) else {
            return;
        };
        // Read the accumulated timings and the final idle interval, then drop
        // the extensions borrow before reading the span name and the store.
        let (busy_ns, idle_ns) = {
            let mut ext = span.extensions_mut();
            let Some(t) = ext.get_mut::<Timings>() else {
                return;
            };
            let now = Instant::now();
            t.idle += now.saturating_duration_since(t.last);
            (t.busy.as_nanos(), t.idle.as_nanos())
        };
        let name = span.name();
        let mut aggs = store.aggs.lock().unwrap();
        let entry = aggs.entry(name).or_default();
        entry.count += 1;
        entry.busy_ns += busy_ns;
        entry.idle_ns += idle_ns;
    }
}

/// The layer to install in the CLI's global subscriber (`init_logging`).
pub fn layer() -> BoxedLayer {
    Box::new(SpanTimingLayer)
}

/// Arm the layer: drop any prior aggregates and start recording closed spans.
pub fn arm() {
    let store = store();
    store.aggs.lock().unwrap().clear();
    store.enabled.store(true, Ordering::Relaxed);
}

/// One serialized row of the per-stage breakdown (milliseconds).
#[derive(Serialize)]
struct SpanRow {
    span: &'static str,
    count: u64,
    total_ms: f64,
    busy_ms: f64,
    idle_ms: f64,
}

/// Disarm, then log a compact table and write `<profile_dir>/<phase>.telemetry.json`.
///
/// No-op-safe: if nothing was recorded (e.g. no spans fired) it still writes an
/// empty array so the artifact set is predictable.
pub fn finish_phase(profile_dir: &str, phase: &str) {
    let store = store();
    store.enabled.store(false, Ordering::Relaxed);
    let snapshot: Vec<(&'static str, SpanAgg)> = {
        let aggs = store.aggs.lock().unwrap();
        aggs.iter().map(|(k, v)| (*k, v.clone())).collect()
    };

    let mut rows: Vec<SpanRow> = snapshot
        .into_iter()
        .map(|(span, a)| SpanRow {
            span,
            count: a.count,
            total_ms: (a.busy_ns + a.idle_ns) as f64 / 1e6,
            busy_ms: a.busy_ns as f64 / 1e6,
            idle_ms: a.idle_ns as f64 / 1e6,
        })
        .collect();
    // Highest total wall-clock first.
    rows.sort_by(|a, b| {
        b.total_ms
            .partial_cmp(&a.total_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    tracing::info!("telemetry[{phase}] per-span wall-clock (busy=on-CPU, idle=await), top spans:");
    for r in rows.iter().take(12) {
        tracing::info!(
            "  {:>9.1}ms total  {:>9.1}ms busy  {:>9.1}ms idle  x{:<4}  {}",
            r.total_ms,
            r.busy_ms,
            r.idle_ms,
            r.count,
            r.span
        );
    }

    if let Err(e) = std::fs::create_dir_all(profile_dir) {
        tracing::warn!("telemetry: cannot create dir '{profile_dir}': {e}");
        return;
    }
    let path = format!("{profile_dir}/{phase}.telemetry.json");
    match serde_json::to_string_pretty(&rows) {
        Ok(json) => match std::fs::write(&path, json) {
            Ok(()) => tracing::info!("telemetry: wrote {path}"),
            Err(e) => tracing::warn!("telemetry: cannot write '{path}': {e}"),
        },
        Err(e) => tracing::warn!("telemetry: serialize failed for {phase}: {e}"),
    }
}
