#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Pins `cognee-cognify`'s duplicated default-user constant to the setting it
//! duplicates.
//!
//! `cognee-cognify` cannot read `Settings` — `cognee-lib` depends on that crate,
//! not the other way round — so `DEFAULT_LEDGER_USER_ID` is a literal there. It
//! is what an ownership row is attributed to when the caller identified no user,
//! and it is folded into every deterministic row id, so a silent divergence from
//! `settings.default_user_id` would make a later delete or sweep compute ids
//! that match nothing.

use cognee::Settings;
use cognee_cognify::DEFAULT_LEDGER_USER_ID;
use uuid::Uuid;

#[test]
fn default_ledger_user_matches_the_configured_default() {
    let configured = Uuid::parse_str(&Settings::default().default_user_id)
        .expect("settings.default_user_id must be a UUID");
    assert_eq!(
        configured, DEFAULT_LEDGER_USER_ID,
        "the ledger's no-user fallback must be the same user every entry point \
         already passes as owner_id"
    );
}
