//! HTTP-server-side pipeline dispatch machinery.
//!
//! This module owns the glue between `AppState`, the `PipelineRunRegistry`,
//! and the synchronous library functions (`cognify`, `memify`, `remember`,
//! `improve`).  The four P3 routers all call `dispatch_pipeline` with their
//! specific `work` closure; the dispatcher handles the `run_in_background`
//! branching and ID computation so that each router does not repeat the logic.

pub mod dispatch;
