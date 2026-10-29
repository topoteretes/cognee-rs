/**
 * Session ops — getSession / addFeedback / deleteFeedback.
 *
 * All tests are Tier-A: no LLM / no network calls.
 * Config follows the same pattern as other test files:
 *   - MOCK_EMBEDDING=true  → no model downloads
 *   - dummy llm_api_key    → OpenAIAdapter constructs, never reaches network
 *   - isolated temp dirs + in-process SQLite DB
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-sess-"));
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

// ─── suite ────────────────────────────────────────────────────────────────────

describe("session ops (Tier-A — no LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    ({ tmpDir, handle } = makeTempHandle("sessions_tier_a@example.com"));
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    cleanup(tmpDir);
  });

  // ── getSession ────────────────────────────────────────────────────────────

  describe("getSession", () => {
    it("returns an empty array for an unknown session id", async () => {
      const entries = await native.cogneeGetSession(handle, "nonexistent-session-abc");
      expect(Array.isArray(entries)).toBe(true);
      expect(entries.length).toBe(0);
    });

    it("accepts lastN option without error", async () => {
      const entries = await native.cogneeGetSession(handle, "no-such-session", {
        lastN: 5,
      });
      expect(Array.isArray(entries)).toBe(true);
    });

    it("returns entries after a rememberEntry call", async () => {
      const sessionId = `sess-test-${Date.now()}`;
      // Store a QA entry so the session has content.
      await native.cogneeRememberEntry(
        handle,
        {
          type: "qa",
          question: "What is TypeScript?",
          answer: "A typed superset of JavaScript.",
        },
        "session_test_ds",
        sessionId
      );
      const entries = await native.cogneeGetSession(handle, sessionId);
      expect(Array.isArray(entries)).toBe(true);
      // The stored QA entry must now appear in the session.
      expect(entries.length).toBeGreaterThanOrEqual(1);
    });
  });

  // ── addFeedback / deleteFeedback ─────────────────────────────────────────

  describe("addFeedback", () => {
    it("returns a boolean for a nonexistent QA entry", async () => {
      const ok = await native.cogneeAddFeedback(
        handle,
        "test-session",
        "00000000-0000-0000-0000-000000000001",
        "nice",
        4
      );
      expect(typeof ok).toBe("boolean");
    });

    it("addFeedback without optional text/score does not throw", async () => {
      await expect(
        native.cogneeAddFeedback(
          handle,
          "test-session",
          "00000000-0000-0000-0000-000000000002"
        )
      ).resolves.toEqual(expect.any(Boolean));
    });
  });

  describe("deleteFeedback", () => {
    it("returns a boolean for a nonexistent QA entry", async () => {
      const ok = await native.cogneeDeleteFeedback(
        handle,
        "test-session",
        "00000000-0000-0000-0000-000000000003"
      );
      expect(typeof ok).toBe("boolean");
    });
  });

  // ── the graph-context snapshot ops are gone ──────────────────────────────

  describe("graph context", () => {
    it("the removed getGraphContext / setGraphContext natives are not exported", () => {
      const exported = native as unknown as Record<string, unknown>;
      expect(exported.cogneeGetGraphContext).toBeUndefined();
      expect(exported.cogneeSetGraphContext).toBeUndefined();
    });
  });
});
