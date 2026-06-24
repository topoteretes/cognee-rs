/**
 * Visualization ops — cogneeVisualize / cogneeVisualizeToFile.
 *
 * All tests are Tier-A: no LLM / no network calls.
 * The visualization feature is tested on an empty graph (no data ingested)
 * which is sufficient to verify the HTML rendering path and the feature-flag
 * behaviour.
 *
 * When the `visualization` Cargo feature is absent, both ops throw a typed
 * error with `code = "FEATURE_NOT_BUILT"`.  The suite handles that case so
 * it stays green on stripped builds.
 *
 * Required env:
 *   MOCK_EMBEDDING=true  → avoids downloading an ONNX model
 *   llm_api_key = "test-dummy-key"  → OpenAIAdapter constructs, never calls network
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-viz-"));
  const dbPath = path.join(tmpDir, "cognee.db");
  const handle = native.cogneeNew({
    system_root_directory: tmpDir,
    data_root_directory: path.join(tmpDir, "data"),
    relational_db_url: `sqlite:${dbPath}?mode=rwc`,
    embedding_provider: "mock",
    llm_api_key: "test-dummy-key",
    default_user_email: email,
  });
  return { tmpDir, handle };
}

function cleanup(dir: string) {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

function errorCode(err: unknown): string | null {
  if (err && typeof err === "object" && "code" in err) {
    return (err as { code: string }).code;
  }
  return null;
}

// ─── suite ────────────────────────────────────────────────────────────────────

describe("visualization ops (Tier-A — no LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    ({ tmpDir, handle } = makeTempHandle("viz_tier_a@example.com"));
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    cleanup(tmpDir);
  });

  // ── cogneeVisualize ───────────────────────────────────────────────────────

  describe("cogneeVisualize", () => {
    it("returns an HTML string or throws FEATURE_NOT_BUILT on an empty graph", async () => {
      let result: string | undefined;
      let caught: unknown;
      try {
        result = await native.cogneeVisualize(handle);
      } catch (err) {
        caught = err;
      }

      if (caught !== undefined) {
        // Feature not compiled in — the only acceptable code is FEATURE_NOT_BUILT.
        expect(errorCode(caught)).toBe("FEATURE_NOT_BUILT");
      } else {
        // Feature is present.  The result is an HTML string.
        expect(typeof result).toBe("string");
        expect((result as string).length).toBeGreaterThan(0);
      }
    }, 30_000);

    it("HTML output contains DOCTYPE or html tag when visualization is built", async () => {
      let result: string | undefined;
      try {
        result = await native.cogneeVisualize(handle);
      } catch {
        // FEATURE_NOT_BUILT — skip the content check.
        return;
      }
      const lower = (result as string).toLowerCase();
      expect(lower.includes("<!doctype") || lower.includes("<html")).toBe(true);
    }, 30_000);

    it("accepts an empty options object without error", async () => {
      try {
        await native.cogneeVisualize(handle, {});
      } catch (err) {
        expect(errorCode(err)).toBe("FEATURE_NOT_BUILT");
      }
    }, 30_000);
  });

  // ── cogneeVisualizeToFile ─────────────────────────────────────────────────

  describe("cogneeVisualizeToFile", () => {
    it("writes HTML to a file and returns its absolute path", async () => {
      const destPath = path.join(tmpDir, "graph.html");
      let filePath: string | undefined;
      try {
        filePath = await native.cogneeVisualizeToFile(handle, {
          destinationPath: destPath,
        });
      } catch (err) {
        expect(errorCode(err)).toBe("FEATURE_NOT_BUILT");
        return;
      }

      expect(typeof filePath).toBe("string");
      expect(path.isAbsolute(filePath as string)).toBe(true);
      expect(fs.existsSync(filePath as string)).toBe(true);

      const content = fs.readFileSync(filePath as string, "utf8");
      expect(content.length).toBeGreaterThan(0);
    }, 30_000);

    it("writes to default path when destinationPath is not provided", async () => {
      let filePath: string | undefined;
      try {
        filePath = await native.cogneeVisualizeToFile(handle);
      } catch (err) {
        expect(errorCode(err)).toBe("FEATURE_NOT_BUILT");
        return;
      }

      expect(typeof filePath).toBe("string");
      // Clean up the file written to the default path.
      try {
        if (filePath && fs.existsSync(filePath)) {
          fs.unlinkSync(filePath);
        }
      } catch {
        /* best effort */
      }
    }, 30_000);
  });

  // ── argument-validation ───────────────────────────────────────────────────

  describe("argument validation", () => {
    it("cogneeVisualize throws synchronously when handle is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeVisualize();
      }).toThrow();
    });

    it("cogneeVisualizeToFile throws synchronously when handle is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeVisualizeToFile();
      }).toThrow();
    });
  });
});
