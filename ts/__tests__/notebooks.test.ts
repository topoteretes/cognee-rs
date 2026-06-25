/**
 * Notebook ops — listNotebooks / createNotebook / updateNotebook / deleteNotebook.
 *
 * All tests are Tier-A: no LLM / no network calls.
 * Config follows the same pattern as other test files:
 *   - MOCK_EMBEDDING=true  → no model downloads
 *   - dummy llm_api_key    → OpenAIAdapter constructs, never reaches network
 *   - isolated temp dirs + in-process SQLite DB
 *
 * Each test group that modifies state uses a dedicated fresh handle so tests
 * do not interfere with each other.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox, CogneeNotebook } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-nb-"));
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

describe("notebook ops (Tier-A — no LLM)", () => {
  beforeAll(() => {
    process.env.MOCK_EMBEDDING = "true";
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
  });

  // ── listNotebooks ─────────────────────────────────────────────────────────

  describe("listNotebooks", () => {
    it("seeds tutorial notebooks on first call", async () => {
      const h = makeTempHandle("nb_list_a@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const notebooks = await native.cogneeListNotebooks(h.handle);
        expect(Array.isArray(notebooks)).toBe(true);
        // Tutorial seeder inserts at least 2 notebooks on the first list call.
        expect(notebooks.length).toBeGreaterThanOrEqual(2);
        for (const nb of notebooks) {
          expect(typeof nb.id).toBe("string");
          expect(typeof nb.name).toBe("string");
        }
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("is idempotent — second call returns the same count", async () => {
      const h = makeTempHandle("nb_list_idem@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const first = await native.cogneeListNotebooks(h.handle);
        const second = await native.cogneeListNotebooks(h.handle);
        expect(second.length).toBe(first.length);
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── createNotebook ────────────────────────────────────────────────────────

  describe("createNotebook", () => {
    it("creates a notebook with a name and returns it", async () => {
      const h = makeTempHandle("nb_create_a@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(
          h.handle,
          "Test Notebook Alpha"
        );
        expect(typeof nb.id).toBe("string");
        expect(nb.id.length).toBeGreaterThan(0);
        expect(nb.name).toBe("Test Notebook Alpha");
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("creates a notebook with cells and deletable=true", async () => {
      const h = makeTempHandle("nb_create_b@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(
          h.handle,
          "Cells Notebook",
          [{ type: "code", content: "console.log('hello')" }],
          true
        );
        expect(nb.name).toBe("Cells Notebook");
        expect(nb.deletable).toBe(true);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("created notebook appears in listNotebooks", async () => {
      const h = makeTempHandle("nb_create_list@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const created = await native.cogneeCreateNotebook(h.handle, "Listed NB");
        const all = await native.cogneeListNotebooks(h.handle);
        const ids = all.map((nb: CogneeNotebook) => nb.id);
        expect(ids).toContain(created.id);
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── updateNotebook ────────────────────────────────────────────────────────

  describe("updateNotebook", () => {
    it("updates the name and returns the updated notebook", async () => {
      const h = makeTempHandle("nb_update_a@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const created = await native.cogneeCreateNotebook(
          h.handle,
          "Original Name"
        );
        const updated = await native.cogneeUpdateNotebook(
          h.handle,
          created.id,
          { name: "Renamed Notebook" }
        );
        expect(updated).not.toBeNull();
        expect(updated?.name).toBe("Renamed Notebook");
        expect(updated?.id).toBe(created.id);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("returns null for a nonexistent notebook id", async () => {
      const h = makeTempHandle("nb_update_miss@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneeUpdateNotebook(
          h.handle,
          "00000000-0000-0000-0000-000000000099",
          { name: "Ghost Notebook" }
        );
        expect(result).toBeNull();
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── deleteNotebook ────────────────────────────────────────────────────────

  describe("deleteNotebook", () => {
    it("deletes an existing notebook and returns true", async () => {
      const h = makeTempHandle("nb_delete_a@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(h.handle, "To Delete");
        const removed = await native.cogneeDeleteNotebook(h.handle, nb.id);
        expect(removed).toBe(true);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("deleted notebook no longer appears in listNotebooks", async () => {
      const h = makeTempHandle("nb_delete_verify@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(h.handle, "Disappear");
        await native.cogneeDeleteNotebook(h.handle, nb.id);
        const all = await native.cogneeListNotebooks(h.handle);
        const ids = all.map((n: CogneeNotebook) => n.id);
        expect(ids).not.toContain(nb.id);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("returns false for a nonexistent notebook id", async () => {
      const h = makeTempHandle("nb_delete_miss@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const removed = await native.cogneeDeleteNotebook(
          h.handle,
          "00000000-0000-0000-0000-000000000099"
        );
        expect(removed).toBe(false);
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });
});
