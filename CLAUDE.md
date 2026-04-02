# cognee-rust development guide

## Build & lint commands

1. Check compilation: `cargo check --all-targets`
2. Run tests (debug): `cargo test`
3. Clippy (must pass — matches CI): `cargo clippy --all-targets --all-features -- -D warnings`
4. Format: `cargo fmt`

**Before committing**, always run clippy and fmt in this order:
```
cargo clippy --allow-dirty --fix --all-targets && cargo fmt
```

Then verify the CI-exact clippy command passes with no errors:
```
cargo clippy --all-targets --all-features -- -D warnings
```
