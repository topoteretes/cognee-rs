//! Password hash migration tests.
//!
//! Verifies that:
//! - The bcrypt fixture (from `python_bcrypt_hash.txt`) authenticates the password.
//! - The argon2id fixture (from `python_argon2_hash.txt`) verifies correctly.
//! - bcrypt verification returns `NeedsRehash`.
//! - argon2id verification returns `Ok`.

mod support;

use cognee_http_server::auth::password::{VerifyOutcome, verify_password};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/auth/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("read fixture {name}"))
        .trim()
        .to_owned()
}

const PASSWORD: &str = "correct horse battery staple";

#[test]
fn bcrypt_fixture_authenticates() {
    let hash = fixture("python_bcrypt_hash.txt");
    assert!(hash.starts_with("$2b$"), "expected bcrypt hash: {hash}");
    let outcome = verify_password(&hash, PASSWORD).expect("verify");
    assert_eq!(
        outcome,
        VerifyOutcome::NeedsRehash,
        "bcrypt must return NeedsRehash"
    );
}

#[test]
fn bcrypt_fixture_rejects_wrong_password() {
    let hash = fixture("python_bcrypt_hash.txt");
    let result = verify_password(&hash, "wrong password");
    assert!(result.is_err(), "wrong password must fail");
}

#[test]
fn argon2id_fixture_authenticates() {
    let hash = fixture("python_argon2_hash.txt");
    assert!(
        hash.starts_with("$argon2id$"),
        "expected argon2id hash: {hash}"
    );
    let outcome = verify_password(&hash, PASSWORD).expect("verify argon2id fixture");
    assert_eq!(outcome, VerifyOutcome::Ok, "argon2id must return Ok");
}

#[test]
fn argon2id_fixture_rejects_wrong_password() {
    let hash = fixture("python_argon2_hash.txt");
    let result = verify_password(&hash, "wrong password");
    assert!(result.is_err(), "wrong password must fail argon2id");
}
