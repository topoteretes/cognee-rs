# Task 06-09 — Wire `COGNEE_LOGS_DIR` through Android run + demo scripts

**Status**: implemented in commit 1172ab6
**Owner**: _unassigned_
**Depends on**: [Task 06-05 — init_logging](05-init-logging.md) (the binary must honour `COGNEE_LOGS_DIR`).
**Blocks**: —

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 10 (Android demo wires `COGNEE_LOGS_DIR` automatically).

---

## 1. Goal

Update [`scripts/android-run.sh`](../../../scripts/android-run.sh)
and [`demo/run_cognee_rust_demo_android.sh`](../../../demo/run_cognee_rust_demo_android.sh)
so the cognee CLI running on the device writes log files to
`/data/local/tmp/cognee/runtime/logs/` automatically. The change is
purely script-side — no binary or library changes.

## 2. Rationale

- The Android device's adb-shell `$HOME` resolves to `/root`, which
  is read-only (see
  [`MEMORY.md` — Android Runtime Fixes](file:///home/dmytro/.claude/projects/-home-dmytro-dev-cognee-cognee-rust/memory/MEMORY.md)).
  Without an explicit `COGNEE_LOGS_DIR`, `resolve_logs_dir` would
  fall through to `/tmp/cognee_logs` — also read-only on Android.
- The runtime base path
  `/data/local/tmp/cognee/runtime/` is already writable and already
  used for the SQLite DB / graph / vectors (see MEMORY.md "Device
  Paths"). Putting logs under `runtime/logs/` keeps everything
  rooted in one writable tree.
- Decision 10 chose to do the env wiring in scripts (not in the
  binary). Reason: the binary's resolver is platform-agnostic;
  encoding Android paths in `cognee-logging` would couple a library
  crate to a deployment target.

## 3. Pre-conditions

- Task 06-05 committed so the binary honours `COGNEE_LOGS_DIR`.
- Scripts at the paths in §1 have not been refactored between
  capture (this doc) and execution. Sub-agent A re-verifies the
  `adb shell` env-export block matches the snippet in §4.

### Current state — `scripts/android-run.sh` (lines 156–166)

```bash
"${ADB}" shell \
    "mkdir -p ${DEVICE_DIR}/tmp && \
     cd ${DEVICE_DIR} && \
     HOME=${DEVICE_DIR} \
     TMPDIR=${DEVICE_DIR}/tmp \
     LLVM_PROFILE_FILE=${DEVICE_DIR}/default.profraw \
     PATH=${DEVICE_DIR}/bin:\$PATH \
     LD_LIBRARY_PATH=${DEVICE_DIR}/lib \
     ORT_DYLIB_PATH=${DEVICE_DIR}/lib/libonnxruntime.so \
     RUST_LOG=${RUST_LOG} \
     ${DEVICE_BINARY}${ARGS_STR}"
```

## 4. Step-by-step

### 4.1 Extend `scripts/android-run.sh`

Edit the `adb shell` block (currently at lines 156–166):

```bash
"${ADB}" shell \
    "mkdir -p ${DEVICE_DIR}/tmp && \
     mkdir -p ${DEVICE_DIR}/runtime/logs && \
     cd ${DEVICE_DIR} && \
     HOME=${DEVICE_DIR} \
     TMPDIR=${DEVICE_DIR}/tmp \
     COGNEE_LOGS_DIR=${DEVICE_DIR}/runtime/logs \
     LLVM_PROFILE_FILE=${DEVICE_DIR}/default.profraw \
     PATH=${DEVICE_DIR}/bin:\$PATH \
     LD_LIBRARY_PATH=${DEVICE_DIR}/lib \
     ORT_DYLIB_PATH=${DEVICE_DIR}/lib/libonnxruntime.so \
     RUST_LOG=${RUST_LOG} \
     ${DEVICE_BINARY}${ARGS_STR}"
```

Two changes: extra `mkdir -p ${DEVICE_DIR}/runtime/logs` (idempotent;
costs nothing if it already exists) and `COGNEE_LOGS_DIR=...` env
export.

Also update the banner (lines ~140–148) to print the new variable:

```bash
echo "=== Android Run ==="
echo "  Binary:   ${BINARY}"
echo "  Args:     ${ARGS_STR:-<none>}"
echo "  RUST_LOG: ${RUST_LOG}"
echo "  Device:   ${DEVICE_DIR}"
echo "  Logs:     ${DEVICE_DIR}/runtime/logs"
# ...
```

### 4.2 Extend `demo/run_cognee_rust_demo_android.sh`

The demo orchestrator at
[`demo/run_cognee_rust_demo_android.sh`](../../../demo/run_cognee_rust_demo_android.sh)
delegates to `scripts/android-run.sh` (line 162). It does **not**
need a separate env export — the underlying `android-run.sh` change
covers all invocations.

What the demo *does* need: a post-run hint that points the user at
the produced logs. Append a one-liner at the end of the demo's
final `printf` summary (search for the closing banner around the
"Demo complete" marker):

```bash
echo "  Logs: adb pull ${DEVICE_DIR}/runtime/logs ./android-demo-logs"
```

If the demo doesn't have a closing summary, skip this step — it's
ergonomics, not correctness.

### 4.3 Verify with a dry run

```bash
# Without an Android device, just check shell syntax.
bash -n scripts/android-run.sh
bash -n demo/run_cognee_rust_demo_android.sh
```

If an Android device is attached:

```bash
PATH="$HOME/Android/Sdk/platform-tools:$PATH" \
    ./scripts/android-run.sh --log info cognee_cli -- --help

# Then verify logs landed:
PATH="$HOME/Android/Sdk/platform-tools:$PATH" \
    adb shell ls /data/local/tmp/cognee/runtime/logs/
```

## 5. Verification

```bash
# 1. Shell syntax is clean (no Android device needed).
bash -n scripts/android-run.sh
bash -n demo/run_cognee_rust_demo_android.sh

# 2. shellcheck (the repo runs this in CI — verify).
shellcheck scripts/android-run.sh
shellcheck demo/run_cognee_rust_demo_android.sh

# 3. Full check.
scripts/check_all.sh

# 4. Manual smoke (device required, optional):
PATH="$HOME/Android/Sdk/platform-tools:$PATH" \
    ./scripts/android-run.sh --log info cognee_cli -- --help && \
adb shell ls /data/local/tmp/cognee/runtime/logs/*.log
```

## 6. Files modified

- [`scripts/android-run.sh`](../../../scripts/android-run.sh) — add
  `mkdir -p` for logs subdir + `COGNEE_LOGS_DIR` env export + banner
  line.
- [`demo/run_cognee_rust_demo_android.sh`](../../../demo/run_cognee_rust_demo_android.sh)
  — (optional) closing-summary hint about pulling logs.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `mkdir -p` fails on a device with `/data/local/tmp` not writable | Very low — same path is already used by the demo for `runtime/` | If it does, the subsequent CLI invocation falls back to `/tmp/cognee_logs` (which is also not writable on Android), then to `None`. File logging silently disables; stdout still works. |
| `${DEVICE_DIR}` interpolation breaks with paths containing spaces | Low — `DEVICE_DIR` is `/data/local/tmp/cognee`; no spaces | Existing script convention; no change. |
| Disk pressure from accumulated log files on long-lived demos | Low — `COGNEE_LOG_MAX_FILES=10` default plus `Rotation::DAILY` keeps it bounded | Document in `README` or the demo script header: "logs are kept for ~10 days; set `COGNEE_LOG_MAX_FILES` to override." |

## 8. Out of scope

- Doing the same wiring for the host-side demo
  (`demo/run_cognee_rust_demo.sh`). The host demo respects user
  preferences — if the user wants logs under `~/.cognee/logs`,
  that's the default; if not, they set their own `COGNEE_LOGS_DIR`.
- Adding an `adb pull` invocation that auto-fetches logs after a
  demo run. Users can do this themselves; the closing hint
  documents the command.
- Wiring `COGNEE_LOG_ROTATION=hourly` for noisy demo runs. The
  default daily rotation is fine for demo workloads.
