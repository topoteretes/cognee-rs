//! Panic hook installed by `cg_init`/`cg_init_with_threads`.
//!
//! Writes a single-line `[cognee-capi panic]` record to stderr with
//! the panic message and location. Coexists with
//! `cognee_setup_logging` — both can be active simultaneously.
//!
//! Installation is one-shot: subsequent `cg_init` calls do not
//! replace the hook so a host application may install its own
//! panic handler for non-cognee panics.

use std::io::Write;
use std::sync::OnceLock;

static INSTALLED: OnceLock<()> = OnceLock::new();

pub(crate) fn install_once() {
    INSTALLED.get_or_init(|| {
        std::panic::set_hook(Box::new(|info| {
            // Resolve the panic message into a borrowed &str when
            // possible — &'static str payloads are the most common
            // (panic!("foo") form), with String falling back via
            // downcast.
            let msg: &str = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("<no message>");

            let loc = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());

            // Single-line stderr write; ignore failures (we are in
            // a panic context, there is nothing to recover to).
            let line = format!("[cognee-capi panic] {msg} at {loc}\n");
            let _ = std::io::stderr().write_all(line.as_bytes());
        }));
    });
}
