/**
 * Smoke tests for the cognee-ts-neon setupLogging() entrypoint
 * (gap-06, decision 9).
 *
 * These tests verify that calling the argument-less binding does not
 * throw and that subsequent calls are idempotent. Configuration is
 * read entirely from environment variables — we point
 * `COGNEE_LOGS_DIR` at a per-test tmpdir before each call.
 *
 * Note: `setupLogging` installs a process-global tracing subscriber;
 * once the first test in the Jest worker installs it, later tests in
 * the same worker hit the idempotent no-op branch. That is by
 * design.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { setupLogging } from "../src";

describe("setupLogging", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-logging-"));
    process.env.COGNEE_LOGS_DIR = tmpDir;
    delete process.env.LOG_FILE_NAME;
  });

  afterEach(() => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // Best effort — the appender worker may still be holding the
      // file open on some platforms.
    }
  });

  it("does not throw on first call", () => {
    expect(() => setupLogging()).not.toThrow();
  });

  it("is idempotent on subsequent calls", () => {
    setupLogging();
    expect(() => setupLogging()).not.toThrow();
  });
});
