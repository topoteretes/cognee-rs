/**
 * Verify the per-binding analytics policy for the Neon binding
 * (gap 07 decision 11).
 *
 * Neon defaults analytics **ON**: ``setupTelemetryAnalytics()`` returns
 * ``true`` unless ``TELEMETRY_DISABLED`` is set, ``ENV`` is
 * ``"test"``/``"dev"``, or ``COGNEE_HOST_SDK`` is set — and
 * ``setTelemetryEnabled`` overrides the environment, process-wide. Each
 * scenario spawns its own child process because the environment is
 * process-wide and the switch is process-wide too.
 */
import { spawnSync } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { resolve } from "path";

// Children `require` the package root so Node resolves
// `package.json` → `main` (compiled `lib/index.js`).
const PACKAGE_ROOT = JSON.stringify(resolve(__dirname, ".."));

function runScript(env: Record<string, string>, script: string): string {
  // Wipe inherited env vars that could shadow the per-test scenario.
  const baseEnv = { ...process.env };
  delete baseEnv.TELEMETRY_DISABLED;
  delete baseEnv.COGNEE_HOST_SDK;
  delete baseEnv.ENV;
  delete baseEnv.LOG_FILE_NAME;
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
  return res.stdout.trim();
}

function runChild(env: Record<string, string>): boolean {
  const script = `
    const cog = require(${PACKAGE_ROOT});
    process.stdout.write(String(cog.setupTelemetryAnalytics()));
  `;
  return runScript(env, script) === "true";
}

// Flip the process-wide switch off and on, reporting the state each time.
const SWITCH = `
  const cog = require(${PACKAGE_ROOT});
  const out = [cog.isTelemetryEnabled()];
  cog.setTelemetryEnabled(false);
  out.push(cog.isTelemetryEnabled(), cog.setupTelemetryAnalytics());
  cog.setTelemetryEnabled(true);
  out.push(cog.isTelemetryEnabled());
  process.stdout.write(out.join(","));
`;

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

describe("setTelemetryEnabled (Neon, process-wide)", () => {
  it("switches analytics off and on from the default", () => {
    expect(runScript({}, SWITCH)).toBe("true,false,false,true");
  });

  it("honours an opt-out set after load, under the switch", () => {
    const script = `
      const cog = require(${PACKAGE_ROOT});
      const out = [cog.isTelemetryEnabled()];
      process.env.TELEMETRY_DISABLED = "1";
      out.push(cog.isTelemetryEnabled());
      cog.setTelemetryEnabled(true);
      out.push(cog.isTelemetryEnabled());
      process.stdout.write(out.join(","));
    `;
    expect(runScript({}, script)).toBe("true,false,true");
  });

  it("wins over TELEMETRY_DISABLED in the environment", () => {
    expect(runScript({ TELEMETRY_DISABLED: "1" }, SWITCH)).toBe(
      "false,false,false,true",
    );
  });
});

describe("process config options (Neon)", () => {
  it("setupTelemetry rejects an unknown option", () => {
    const out = runScript(
      {},
      `
        const cog = require(${PACKAGE_ROOT});
        try { cog.setupTelemetry({ otelNope: "x" }); process.stdout.write("no-throw"); }
        catch (e) { process.stdout.write(String(e.message).includes("unknown OpenTelemetry setting") ? "threw" : String(e.message)); }
      `,
    );
    expect(out).toBe("threw");
  });

  it("setupLogging options win over COGNEE_LOGS_DIR", () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-logopts-"));
    const fromEnv = path.join(root, "from_env");
    const fromCode = path.join(root, "from_code");
    try {
      runScript(
        // Suppress the load-time bridge so setupLogging's subscriber installs.
        { COGNEE_LOGS_DIR: fromEnv, COGNEE_BINDING_SUPPRESS_LOGS: "1" },
        `
          const cog = require(${PACKAGE_ROOT});
          cog.setupLogging({ logsRootDirectory: ${JSON.stringify(fromCode)}, logRotation: "never" });
        `,
      );
      expect(fs.existsSync(fromCode)).toBe(true);
      expect(fs.readdirSync(fromCode).length).toBeGreaterThan(0);
      expect(fs.existsSync(fromEnv)).toBe(false);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
