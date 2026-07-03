# ONNX Runtime CI mirror

## Why this exists

Every build that enables the `onnx` feature (the TS Neon prebuilds, the C-API
release, `community.yml`, `record-cassettes.yml`, …) pulls in `ort-sys`, whose
build script downloads a prebuilt ONNX Runtime archive from `cdn.pyke.io` at
compile time. That CDN is fronted by Cloudflare, which intermittently returns
**HTTP 403** to GitHub Actions (Azure) runner IP ranges. On any ort-cache miss
the build then dies with:

```
error: ort-sys@2.0.0-rc.12: ort-sys failed to download prebuilt binaries from
`https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/<target>.tar.lzma2`: http status: 403
```

The URL and artifact are fine (they return 200 from most networks) — it is a
runner-IP block, so it strikes unpredictably and cannot be fixed by retrying.

## How the mirror works

`ort-sys` links the ONNX Runtime it extracts into
`<ORT_CACHE_DIR>/dfbin/<target>/<sha256>/` and **skips the network download
entirely when that directory already exists** (`ort-sys/build/main.rs`). We
exploit that: we host our own copy of the binaries and pre-populate the cache
directory before `cargo build` runs.

- **`ci/ort/runtime.lock`** — the single source of truth: the pinned `ort`
  version, the ONNX Runtime version, the mirror release tag, and the per-target
  `<sha256>` (copied verbatim from `ort-sys/build/download/dist.txt`, the `none`
  / CPU row). That sha256 is the `dfbin` cache-directory name the mirror must
  populate.
- **`ci/ort/prefetch.sh` (consumer)** — reads the lock, and if the cache dir is
  cold, downloads `ort-<target>.tar.gz` from the mirror release and extracts it
  into `dfbin/<target>/<sha256>/`. Best-effort: on any failure it exits 0 and
  the build falls back to the normal pyke download, so it can never make CI
  worse than it is today. Wired into `ts-prebuild.yml` before the build step.
- **`.github/workflows/ort-mirror.yml` (producer)** — a `workflow_dispatch`
  maintenance job. On each platform runner it does a real build so `ort-sys`
  downloads and decodes the pyke archive (the `.tar.lzma2` is a raw LZMA2
  stream that only `ort-sys`'s bundled decoder reads), then repackages the
  extracted cache directory as a plain `ort-<target>.tar.gz` and uploads it to
  the release tag from the lock.

The mirror asset is a `.tar.gz` of the *extracted* runtime (not the raw
`.tar.lzma2`), so the consumer needs nothing but `curl` + `tar`.

## First-time setup / activation

Until the mirror release assets exist, `prefetch.sh` simply no-ops and builds
download from pyke as before. To activate the mirror:

1. Run the **ORT Runtime Mirror** workflow (Actions → *ORT Runtime Mirror* →
   *Run workflow*). Run it when pyke is reachable from the runners (i.e. not
   during an active 403 window). It creates the `ort-runtime-<version>` release
   and uploads one `ort-<target>.tar.gz` per platform.
2. That's it — subsequent builds hit the mirror first.

## Refreshing when `ort` is bumped

The pinned sha256 hashes are tied to the exact `ort` / ONNX Runtime version.
When bumping `ort`:

1. Update the `ort` version in the workspace `Cargo.toml` / lockfiles.
2. Update **`ci/ort/runtime.lock`**: the `VERSION` line (crate version, ONNX
   Runtime version, and a new `ort-runtime-<new-version>` tag) and every
   per-target sha256. Get the new hashes from the `none` rows of
   `ort-sys/build/download/dist.txt` in the new `ort-sys` source
   (`~/.cargo/registry/src/*/ort-sys-<ver>/build/download/dist.txt`).
3. Re-run the **ORT Runtime Mirror** workflow to publish assets under the new
   tag.

If a hash in the lock is wrong, the producer fails loudly (the expected `dfbin`
dir won't exist) and the consumer simply falls back to pyke — no silent
mislinking.

## Extending to other workflows

`prefetch.sh` is self-contained. To protect another `onnx`-building workflow,
add one step before its build, pointed at the same `ORT_CACHE_DIR` that build
uses:

```yaml
- name: Prefetch ONNX Runtime from mirror
  if: runner.os != 'Windows'
  shell: bash
  run: ci/ort/prefetch.sh "<rust-target>" "<ort-cache-dir>"
```
