/**
 * Session ops — getSession / addFeedback / deleteFeedback /
 *               getGraphContext / setGraphContext.
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

  // ── getGraphContext / setGraphContext ─────────────────────────────────────

  describe("graph context", () => {
    it("getGraphContext returns null for an unknown session", async () => {
      const ctx = await native.cogneeGetGraphContext(handle, "no-such-session-ctx");
      expect(ctx).toBeNull();
    });

    it("setGraphContext + getGraphContext round-trip preserves the payload", async () => {
      const sessionId = `gc-rt-${Date.now()}`;
      const payload = JSON.stringify({ nodes: ["A", "B"], edges: [["A", "B"]] });

      await native.cogneeSetGraphContext(handle, sessionId, payload);
      const retrieved = await native.cogneeGetGraphContext(handle, sessionId);
      expect(retrieved).toBe(payload);
    });

    it("setGraphContext overwrites a previous value", async () => {
      const sessionId = `gc-overwrite-${Date.now()}`;
      await native.cogneeSetGraphContext(handle, sessionId, "first");
      await native.cogneeSetGraphContext(handle, sessionId, "second");
      const retrieved = await native.cogneeGetGraphContext(handle, sessionId);
      expect(retrieved).toBe("second");
    });

    it("setGraphContext with an empty string stores an empty string", async () => {
      const sessionId = `gc-empty-${Date.now()}`;
      await native.cogneeSetGraphContext(handle, sessionId, "");
      const retrieved = await native.cogneeGetGraphContext(handle, sessionId);
      // Either null or empty string is acceptable for an empty payload.
      expect(retrieved === null || retrieved === "").toBe(true);
    });
  });

  // ── two handles share no session state ───────────────────────────────────

  describe("session isolation", () => {
    it("two handles do not share session state", async () => {
      const h2 = makeTempHandle("sess_isolated@example.com");
      await native.cogneeWarm(h2.handle);
      try {
        const sessionId = `iso-${Date.now()}`;
        await native.cogneeSetGraphContext(handle, sessionId, "handle1-value");

        // A fresh handle with its own DB must not see the other handle's data.
        const ctx = await native.cogneeGetGraphContext(h2.handle, sessionId);
        expect(ctx).toBeNull();
      } finally {
        cleanup(h2.tmpDir);
      }
    });
  });
});
