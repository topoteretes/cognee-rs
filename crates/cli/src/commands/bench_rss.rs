//! Peak resident-set-size sampling for `cognee-cli bench` (SDK-507).
//!
//! SDK-507 asks whether cognify's in-memory retention grows with the corpus
//! fast enough to threaten a fixed-size box, and gates its per-wave-flush and
//! streaming tiers on a **bytes-per-document slope**. Nothing in the repo could
//! produce one: there is no allocator hook, no `dhat`, and `bench` measured
//! wall-clock only. This module adds the missing y-axis — the process peak RSS,
//! read at each bench phase boundary — so that a `--num-memories` sweep over a
//! single corpus yields the slope with no API key and no production run.
//!
//! # What the number is, precisely
//!
//! `getrusage(2)`'s `ru_maxrss` is a **high-water mark over the whole process
//! lifetime**: it never decreases and cannot be reset. A per-phase sample is
//! therefore "the highest RSS this process had reached by the end of that
//! phase", and the difference between two consecutive samples is how much the
//! phase *raised* the high-water mark — not the phase's own peak, and not its
//! retained set. A phase that allocates heavily but stays under an earlier peak
//! reports a delta of zero. For this ticket that is the right instrument: the
//! question is whether the whole-run peak scales with the corpus.
//!
//! Resident set is also not the same as live allocated bytes. Freed pages the
//! allocator has not returned to the OS still count, so RSS is an upper bound
//! on live data. That is the conservative direction for an OOM question.
//!
//! # Why no per-document figure is emitted here
//!
//! Peak RSS at one corpus size is `baseline + slope × corpus`, and the baseline
//! (tokenizer, embedding engine, graph/vector backends, tokio + rayon stacks)
//! is large and fixed. Dividing one run's peak by its document count reports
//! mostly baseline and would overstate the slope by a wide margin at small
//! sizes. The slope only exists across **two or more** corpus sizes, so this
//! module emits raw samples and the regression lives in
//! `scripts/perf/measure_retention.py`.

use serde::Serialize;

/// Whether this platform's `ru_maxrss` is already in bytes.
///
/// The field's unit is **not** portable, and getting it wrong is a silent
/// 1024× error in exactly the number this ticket turns on:
///
/// * Darwin (macOS, iOS) reports **bytes**.
/// * Linux, Android and the BSDs report **kilobytes**.
///
/// Pinned by `maxrss_to_bytes_uses_the_platform_unit` below.
#[cfg(unix)]
const MAXRSS_IS_BYTES: bool = cfg!(any(target_os = "macos", target_os = "ios"));

/// Convert a raw `ru_maxrss` reading to bytes, or `None` if it is unusable.
///
/// Non-positive readings mean the kernel did not fill the field in; report them
/// as unavailable rather than as a real zero-byte peak.
#[cfg(unix)]
const fn maxrss_to_bytes(raw: i64) -> Option<u64> {
    if raw <= 0 {
        return None;
    }
    let raw = raw as u64;
    if MAXRSS_IS_BYTES {
        Some(raw)
    } else {
        Some(raw.saturating_mul(1024))
    }
}

/// Peak resident set size of this process so far, in bytes.
///
/// Returns `None` where the platform cannot report it (non-unix) or the syscall
/// failed, so callers record "unsupported" rather than a fabricated zero.
#[cfg(unix)]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` writes a `struct rusage` through the pointer we give
    // it and reads nothing else. The allocation is a correctly-sized, aligned,
    // zeroed `MaybeUninit<libc::rusage>` that outlives the call, and the return
    // code is checked before `assume_init`.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `getrusage` returned 0, so it initialised the whole struct.
    let usage = unsafe { usage.assume_init() };
    // `ru_maxrss` is `c_long`: already `i64` on every 64-bit unix (where the
    // conversion is a no-op clippy objects to) and `i32` on 32-bit ones (where
    // it is load-bearing). Allowed rather than dropped so the 32-bit build
    // keeps compiling.
    #[allow(
        clippy::useless_conversion,
        reason = "c_long is i32 on 32-bit unix, where this widens"
    )]
    maxrss_to_bytes(i64::from(usage.ru_maxrss))
}

/// Peak resident set size of this process so far, in bytes.
///
/// Always `None` off unix: there is no portable equivalent, and reporting a
/// made-up number would be worse than reporting none.
#[cfg(not(unix))]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    None
}

/// How much a phase raised the process high-water mark, in bytes.
///
/// `None` when either sample is missing. Saturating rather than wrapping
/// because `ru_maxrss` is monotonic in principle but nothing in this process
/// enforces it — a wrapping subtraction would turn a 1-byte anomaly into 18
/// exabytes in the reported JSON.
fn high_water_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    Some(after?.saturating_sub(before?))
}

/// The `memory` block added to the bench result JSON.
///
/// Additive: every field is new, so the Python orchestrator's schema — which
/// reads the keys it knows by name — is unaffected.
#[derive(Debug, Default, Serialize)]
pub(crate) struct MemoryReport {
    /// Whether this platform reported a peak RSS at all. When `false`, every
    /// `peak_rss_*` field below is `null` and the run contributes no y-axis
    /// point — the sweep script skips it rather than fitting through zeros.
    pub peak_rss_supported: bool,

    /// Exact byte length of the document text handed to `add`, summed over the
    /// corpus: one copy of the input.
    ///
    /// This is the same text that lands at each `Data.raw_data_location`, so it
    /// is directly comparable to the "sum the corpus file sizes" arithmetic
    /// SDK-507 proposes for a production corpus — measured rather than
    /// estimated, and it is the x-axis the slope is fitted against.
    pub corpus_bytes: u64,

    /// Number of documents behind `corpus_bytes`. The second x-axis: the ticket
    /// is phrased per *document*, but per *byte* is the more transferable
    /// figure when document sizes differ between corpora.
    pub corpus_documents: usize,

    /// High-water mark after component init and before `add` — the fixed cost
    /// of the process (backends, tokenizer, runtimes) that the regression
    /// intercept should land near. A fitted intercept far from this means the
    /// fit is being driven by something other than corpus size.
    pub peak_rss_bytes_baseline: Option<u64>,

    /// High-water mark after `add`.
    pub peak_rss_bytes_after_add: Option<u64>,

    /// High-water mark after `cognify` — the y-axis for SDK-507.
    pub peak_rss_bytes_after_cognify: Option<u64>,

    /// High-water mark after `search`.
    pub peak_rss_bytes_after_search: Option<u64>,

    /// How much `add` raised the high-water mark.
    pub add_high_water_delta_bytes: Option<u64>,

    /// How much `cognify` raised the high-water mark beyond what `add` had
    /// already reached. Note that `add` runs first and leaves its own peak
    /// standing, so a small delta here does **not** mean cognify is cheap — it
    /// can mean cognify stayed under `add`'s peak. Read the absolute
    /// `peak_rss_bytes_after_cognify` across corpus sizes for the slope; read
    /// this only to attribute which phase moved the peak.
    pub cognify_high_water_delta_bytes: Option<u64>,

    /// Chunks cognify produced, or `None` if the phase failed.
    ///
    /// The ticket's retention model is per *chunk*, not per document — chunk
    /// text is what every stage clones and what the vector term
    /// `chunks x dims x 4 B x copies` is counted in. Documents are only a proxy
    /// for it, and a poor one across corpora with different document sizes.
    pub cognify_chunks: Option<u64>,

    /// Embeddings cognify retained on its result, or `None` if the phase
    /// failed. With `config.embedding_dimensions` this gives the vector term
    /// exactly: `cognify_embeddings x dims x 4 B` per retained copy.
    pub cognify_embeddings: Option<u64>,
}

impl MemoryReport {
    /// Start a report for a corpus of `documents` documents totalling
    /// `corpus_bytes` bytes of document text, sampling the baseline now.
    pub fn new(corpus_bytes: u64, corpus_documents: usize) -> Self {
        let baseline = peak_rss_bytes();
        Self {
            peak_rss_supported: baseline.is_some(),
            corpus_bytes,
            corpus_documents,
            peak_rss_bytes_baseline: baseline,
            ..Self::default()
        }
    }

    /// Record the high-water mark observed at the end of the `add` phase.
    pub fn record_add(&mut self, peak: Option<u64>) {
        self.peak_rss_bytes_after_add = peak;
        self.add_high_water_delta_bytes = high_water_delta(self.peak_rss_bytes_baseline, peak);
    }

    /// Record the high-water mark observed at the end of the `cognify` phase.
    pub fn record_cognify(&mut self, peak: Option<u64>) {
        self.peak_rss_bytes_after_cognify = peak;
        self.cognify_high_water_delta_bytes = high_water_delta(self.peak_rss_bytes_after_add, peak);
    }

    /// Record the high-water mark observed at the end of the `search` phase.
    pub fn record_search(&mut self, peak: Option<u64>) {
        self.peak_rss_bytes_after_search = peak;
    }

    /// Record what cognify produced. Left `None` when the phase failed, so a
    /// failed run contributes no point to the fit rather than a zero one.
    pub fn record_cognify_counts(&mut self, chunks: usize, embeddings: usize) {
        self.cognify_chunks = Some(chunks as u64);
        self.cognify_embeddings = Some(embeddings as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One mebibyte — a floor no real process is under, and a value a
    /// kilobyte-vs-byte mix-up cannot reach on Linux.
    const MIB: u64 = 1024 * 1024;

    #[cfg(unix)]
    #[test]
    fn maxrss_to_bytes_uses_the_platform_unit() {
        // Pinned per platform rather than via the same `cfg!` the code uses,
        // so flipping `MAXRSS_IS_BYTES` fails here instead of agreeing with
        // itself.
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            assert_eq!(maxrss_to_bytes(4096), Some(4096), "Darwin reports bytes");
        } else {
            assert_eq!(
                maxrss_to_bytes(4096),
                Some(4096 * 1024),
                "Linux and the BSDs report kilobytes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn maxrss_to_bytes_rejects_unfilled_readings() {
        assert_eq!(maxrss_to_bytes(0), None);
        assert_eq!(maxrss_to_bytes(-1), None);
    }

    /// The end-to-end unit check: whatever this process's peak really is, a
    /// 1024× error in either direction lands outside this window. A live test
    /// binary sits comfortably between 1 MiB and 8 GiB; on Linux a missing
    /// `×1024` reports tens of thousands of bytes, and on Darwin a spurious one
    /// reports tens of gibibytes.
    #[test]
    fn peak_rss_is_reported_in_plausible_bytes() {
        match peak_rss_bytes() {
            Some(peak) => {
                assert!(
                    (MIB..8 * 1024 * MIB).contains(&peak),
                    "peak RSS {peak} B is outside [1 MiB, 8 GiB) — wrong unit?"
                );
            }
            // Non-unix has no reading; that is a supported outcome, not a bug.
            None => assert!(!cfg!(unix), "unix must report a peak RSS"),
        }
    }

    #[test]
    fn high_water_delta_is_after_minus_before() {
        assert_eq!(high_water_delta(Some(100), Some(250)), Some(150));
    }

    #[test]
    fn high_water_delta_saturates_instead_of_wrapping() {
        // `ru_maxrss` is monotonic in principle; nothing here enforces it.
        assert_eq!(high_water_delta(Some(250), Some(100)), Some(0));
    }

    #[test]
    fn high_water_delta_needs_both_samples() {
        assert_eq!(high_water_delta(None, Some(250)), None);
        assert_eq!(high_water_delta(Some(100), None), None);
    }

    #[test]
    fn report_marks_unsupported_when_no_reading_is_available() {
        let report = MemoryReport::new(1234, 7);
        assert_eq!(report.corpus_bytes, 1234);
        assert_eq!(report.corpus_documents, 7);
        assert_eq!(
            report.peak_rss_supported,
            report.peak_rss_bytes_baseline.is_some()
        );
    }

    #[test]
    fn cognify_delta_is_measured_from_the_add_peak_not_the_baseline() {
        let mut report = MemoryReport {
            peak_rss_bytes_baseline: Some(100),
            ..MemoryReport::default()
        };
        report.record_add(Some(300));
        report.record_cognify(Some(500));

        assert_eq!(report.add_high_water_delta_bytes, Some(200));
        // 500 - 300, not 500 - 100: `add` has already raised the mark.
        assert_eq!(report.cognify_high_water_delta_bytes, Some(200));
        assert_eq!(report.peak_rss_bytes_after_cognify, Some(500));
    }
}
