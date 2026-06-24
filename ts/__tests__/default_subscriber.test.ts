/**
 * Verify the default stderr `tracing` subscriber the Neon binding
 * installs from `#[neon::main]` (gap 07 decisions 1 + 3).
 *
 * The subscriber is process-global; once installed, neither rebuilding
 * the addon nor reloading the module replaces it. To exercise the
 * install path and the `COGNEE_BINDING_SUPPRESS_LOGS` opt-out cleanly
 * we spawn child Node processes — `require`ing the addon fresh per
 * process guarantees a fresh install attempt under the env vars we
 * pass.
 *
 * The subscriber writes to **stderr** at default `info,ort=warn`
 * level (matching the CLI binary). A pipeline execution emits
 * `tracing::info_span!`/`tracing::warn!` events inside `cognee-core`,
 * so we can assert on the stderr output of a child that executes one.
 */
import { spawnSync } from "child_process";
import { resolve } from "path";

// Child processes `require` the package via its package root so they
// pick up `package.json` → `main` (which points at the compiled
// `lib/index.js`). Pointing at `src/` instead would only resolve under
// ts-jest, not a plain Node child.
const PACKAGE_ROOT = JSON.stringify(resolve(__dirname, ".."));

interface ChildOutput {
  status: number | null;
  stderr: string;
  stdout: string;
}

function runChild(env: Record<string, string>): ChildOutput {
  // The pipeline body is intentionally trivial — we only care that
  // executing it triggers a tracing event inside cognee-core. Errors
  // in the body should still surface tracing output before propagating.
  //
  // We exit via ``process.exit(0)`` rather than ``cog.shutdown()`` to
  // avoid blocking on a clean tokio runtime drop — the goal here is
  // to observe the default subscriber's output, not to exercise the
  // shutdown path.
  const script = `
    const cog = require(${PACKAGE_ROOT});
    cog.init();
    (async () => {
      const { context } = cog.TaskContext.mock();
      const p = new cog.Pipeline("default-subscriber-smoke");
      p.addTask(cog.createTask((x) => x + 1, { name: "inc" }));
      const r = await p.execute([1], context);
      process.stdout.write("ok=" + JSON.stringify(r) + "\\n");
      process.exit(0);
    })().catch((e) => { console.error("ERR", e && e.message); process.exit(2); });
  `;
  const res = spawnSync(
    process.execPath,
    ["-e", script],
    {
      env: { ...process.env, ...env },
      encoding: "utf8",
      timeout: 60_000,
    },
  );
  return { status: res.status, stderr: res.stderr ?? "", stdout: res.stdout ?? "" };
}

describe("default subscriber", () => {
  it("writes to stderr at info level by default", () => {
    const out = runChild({ RUST_LOG: "info", COGNEE_BINDING_SUPPRESS_LOGS: "" });
    expect(out.status).toBe(0);
    expect(out.stdout).toContain("ok=");
    // The default fmt subscriber writes `INFO` / `WARN` level tokens
    // in plain text. A pipeline execution emits at least one INFO
    // span event inside cognee-core.
    expect(out.stderr).toMatch(/INFO|WARN/);
  });

  it("COGNEE_BINDING_SUPPRESS_LOGS=1 suppresses the default subscriber", () => {
    const out = runChild({ RUST_LOG: "info", COGNEE_BINDING_SUPPRESS_LOGS: "1" });
    expect(out.status).toBe(0);
    expect(out.stdout).toContain("ok=");
    // With suppression, the default subscriber is NOT installed.
    // The child's stderr should not carry the INFO/WARN markers a
    // tracing-subscriber `fmt` layer would write. We accept any
    // stray bytes other hot paths might write — only the tracing
    // markers must be absent.
    expect(out.stderr).not.toMatch(/\sINFO\s|\sWARN\s/);
  });
});
