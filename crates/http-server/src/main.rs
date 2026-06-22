//! Standalone `cognee-http-server` binary entry point.
//!
//! TEMPORARILY DISABLED — see `lib.rs` for the rationale (T2-move §4 S2).
//!
//! The binary stays in the crate's `[[bin]]` target list so consumers
//! that explicitly enable the `bin` feature still get a buildable
//! artifact; it prints a clear message and exits non-zero so any
//! deployment relying on it discovers the gating loudly rather than
//! silently shipping an empty binary.

fn main() {
    eprintln!(
        "cognee-http-server: temporarily disabled on the oss-split branch. \
         T3 will re-home the auth-table-backed router family and DB wiring \
         inside cognee-cloud-rust. See docs/roadmap/oss-split-plan.md §4 S2."
    );
    std::process::exit(1);
}
