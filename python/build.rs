// Emit the linker arguments a PyO3 extension module needs, so a plain
// `cargo build` of this crate links.
//
// PyO3 does not do this itself: `add_extension_module_link_args` is a public
// helper whose own docs say "This should be called from a build script", and
// pyo3's build script never calls it. This crate had no build script at all,
// so on macOS nothing emitted
//
//     cargo:rustc-cdylib-link-arg=-undefined
//     cargo:rustc-cdylib-link-arg=dynamic_lookup
//
// and the cdylib link failed on every Python symbol it references
// (`_Py_IsInitialized`, `_PyUnicode_*`, … 95 of them). Enabling
// `pyo3/extension-module`, which this crate already does, stops libpython
// being linked but does not tell the linker to tolerate what that leaves
// undefined. See the `[lib]` block in Cargo.toml for the harness settings that
// go with this.
//
// The symptom: `cargo build` and `cargo build --workspace` failed from a clean
// checkout, while `python/scripts/check.sh` passed — maturin passes these flags
// itself, so packaged wheels were never affected — and `scripts/check_all.sh`
// uses `cargo check`, which does not link. Only plain cargo hit it.
//
// Trade-off worth knowing: `-undefined dynamic_lookup` suppresses
// undefined-symbol errors for *every* symbol in this cdylib, not just `_Py*`.
// If a future dependency needs an Apple framework nobody passes to the linker
// — the #116 scenario — this crate will link cleanly and fail instead at
// `import cognee_py._native` with "symbol not found in flat namespace". That
// detection is still active for other packages (`rustc-cdylib-link-arg` is
// scoped to this one), but it is off here, and there is no way to keep it while
// building an extension module this way.
//
// Non-macOS targets: the helper is a no-op except on wasm32-unknown-emscripten,
// so calling it unconditionally is safe.
fn main() {
    // Without this, cargo falls back to "re-run if any file in the package
    // changed", so editing a .py test, a stub or the README would re-run this
    // script and relink the ~270 MB cdylib. The script depends on nothing but
    // itself and the target triple, which cargo already tracks.
    println!("cargo:rerun-if-changed=build.rs");
    pyo3_build_config::add_extension_module_link_args();
}
