# cognee-rust Project Instructions

## Completing Changes

Before marking any set of changes as complete, run the full check suite to ensure nothing is broken:

```bash
scripts/check_all.sh
```

This script verifies:
1. Rust formatting (`cargo fmt --all -- --check`)
2. Rust compilation (`cargo check --all-targets`)
3. Clippy lints (`cargo clippy --all-targets -- -D warnings`)
4. C API bindings — builds with CMake and runs all examples (`capi/scripts/check.sh`)
5. Python bindings — builds with maturin and runs pytest (`python/scripts/check.sh`)
6. JS/TS bindings — builds with npm and runs Jest (`js/scripts/check.sh`)
