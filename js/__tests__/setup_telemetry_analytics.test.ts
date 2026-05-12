/**
 * Verify the per-binding analytics policy for the Neon binding
 * (gap 07 decision 11).
 *
 * Neon defaults analytics **ON**: ``setupTelemetryAnalytics()`` returns
 * ``true`` unless ``TELEMETRY_DISABLED`` is set, ``ENV`` is
 * ``"test"``/``"dev"``, or ``COGNEE_HOST_SDK`` is set. Each scenario
 * spawns its own child process because the binding latches the
 * decision in a ``OnceLock<Mutex<Option<bool>>>`` (decision 12).
 */
import { spawnSync } from "child_process";
import { resolve } from "path";

// Children `require` the package root so Node resolves
// `package.json` → `main` (compiled `lib/index.js`).
const PACKAGE_ROOT = JSON.stringify(resolve(__dirname, ".."));

function runChild(env: Record<string, string>): boolean {
  const script = `
    const cog = require(${PACKAGE_ROOT});
    process.stdout.write(String(cog.setupTelemetryAnalytics()));
  `;
  // Wipe inherited env vars that could shadow the per-test scenario.
  const baseEnv = { ...process.env };
  delete baseEnv.TELEMETRY_DISABLED;
  delete baseEnv.COGNEE_HOST_SDK;
  delete baseEnv.ENV;
  const res = spawnSync(process.execPath, ["-e", script], {
    env: { ...baseEnv, ...env },
    encoding: "utf8",
    timeout: 30_000,
  });
  if (res.status !== 0) {
    throw new Error(
      `child exited with status ${res.status}: stderr=${res.stderr} stdout=${res.stdout}`,
    );
  }
  return res.stdout.trim() === "true";
}

describe("setupTelemetryAnalytics (Neon)", () => {
  it("defaults to ON when no opt-out env is set", () => {
    expect(runChild({})).toBe(true);
  });

  it("TELEMETRY_DISABLED=1 suppresses analytics", () => {
    expect(runChild({ TELEMETRY_DISABLED: "1" })).toBe(false);
  });

  it("ENV=test suppresses analytics", () => {
    expect(runChild({ ENV: "test" })).toBe(false);
  });

  it("ENV=dev suppresses analytics", () => {
    expect(runChild({ ENV: "dev" })).toBe(false);
  });

  it("COGNEE_HOST_SDK=python suppresses analytics", () => {
    expect(runChild({ COGNEE_HOST_SDK: "python" })).toBe(false);
  });
});
