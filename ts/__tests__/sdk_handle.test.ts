/**
 * Tier-A test for the Phase-1 SDK handle + service facade.
 *
 * Constructs a handle and warms it with a fully-local config:
 *   - MOCK_EMBEDDING=true            → no embedding model download / network
 *   - a non-empty dummy llm_api_key  → OpenAIAdapter constructs (no network I/O)
 *   - temp system/data dirs + a temp sqlite DB → isolated, no shared state
 *
 * Asserts:
 *   - `cogneeOwnerId(handle)` resolves to uuid5(NAMESPACE_OID, default_user_email)
 *     (Python default-user semantics).
 *
 * Runs WITHOUT any LLM/network: warming touches the relational DB and builds the
 * mock embedding engine; the LLM engine is constructed but never exercised.
 */
import * as crypto from "crypto";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { Cognee } from "../src/cognee";
import { native } from "../src/native";

/** RFC-4122 UUIDv5 (SHA-1) of `name` under the OID namespace. */
function uuid5Oid(name: string): string {
  // NAMESPACE_OID = 6ba7b812-9dad-11d1-80b4-00c04fd430c8 (matches Rust's
  // uuid::Uuid::NAMESPACE_OID, used by get_or_create_default_user).
  const namespace = "6ba7b812-9dad-11d1-80b4-00c04fd430c8";
  const nsBytes = Buffer.from(namespace.replace(/-/g, ""), "hex");
  const hash = crypto.createHash("sha1");
  hash.update(nsBytes);
  hash.update(Buffer.from(name, "utf8"));
  const bytes = hash.digest().subarray(0, 16);
  // Set version (5) and variant (RFC 4122) bits.
  bytes[6] = (bytes[6] & 0x0f) | 0x50;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = bytes.toString("hex");
  return [
    hex.substring(0, 8),
    hex.substring(8, 12),
    hex.substring(12, 16),
    hex.substring(16, 20),
    hex.substring(20, 32),
  ].join("-");
}

describe("Phase-1 SDK handle & facade", () => {
  let tmpDir: string;
  const email = "phase1_tier_a@example.com";
  let dbPath: string;

  beforeAll(() => {
    process.env.MOCK_EMBEDDING = "true";
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-sdk-handle-"));
    dbPath = path.join(tmpDir, "cognee.db");
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best effort
    }
  });

  function makeSettings() {
    return {
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      // `?mode=rwc` so sqlx creates the file if missing (matches how the
      // CLI/host pre-create the DB; see project notes on sqlx not auto-creating).
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      embedding_provider: "mock",
      // Non-empty dummy key so the OpenAI adapter constructs (no network I/O).
      llm_api_key: "test-dummy-key",
      default_user_email: email,
    };
  }

  it("constructs synchronously and warms, resolving the email-derived owner id", async () => {
    const handle = native.cogneeNew(makeSettings());
    expect(handle).toBeDefined();

    // Warm should not reject (builds engines + runs get_or_create_default_user).
    await expect(native.cogneeWarm(handle)).resolves.toBeUndefined();

    const ownerId = await native.cogneeOwnerId(handle);
    expect(ownerId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    );
    expect(ownerId).toBe(uuid5Oid(email));
  });

  // Note: a former test asserted that a `users` row was created after warm by
  // directly inspecting the SQLite `users` table. T2-move relocated the
  // `users` table to the closed cognee-access-control crate
  // (cognee-cloud-rust/crates/access-control/src/migrator/m20260914_000002_auth.rs);
  // the OSS baseline migration no longer creates a `users` table at all
  // (crates/database/src/migrator/m20260914_000001_baseline.rs). The uuid5
  // parity test above ("resolves owner id lazily without an explicit warm")
  // is the load-bearing default-user assertion in OSS. Closed bindings (T15)
  // will restore the users-row test on the closed side.
  it("resolves owner id lazily without an explicit warm (idempotent)", async () => {
    const handle = native.cogneeNew(makeSettings());
    // No cogneeWarm() — cogneeOwnerId warms on demand.
    const ownerId = await native.cogneeOwnerId(handle);
    expect(ownerId).toBe(uuid5Oid(email));
  });

  // ── close() and the WAL sidecars (issue #132) ──────────────────────────────
  //
  // A warmed handle runs its relational database in WAL mode, which creates
  // `<db>-wal`/`<db>-shm`. Letting the handle be garbage-collected does not
  // reliably remove them: the pool's destructor lets its connections tear down
  // concurrently and SQLite only unlinks the sidecars when the *last* connection
  // closes. `close()` must be deterministic about it.
  describe("close()", () => {
    let closeDir: string;
    let closeDb: string;

    beforeEach(() => {
      closeDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-sdk-close-"));
      closeDb = path.join(closeDir, "cognee.db");
    });

    afterEach(() => {
      try {
        fs.rmSync(closeDir, { recursive: true, force: true });
      } catch {
        // best effort
      }
    });

    function closeSettings() {
      return {
        ...makeSettings(),
        system_root_directory: closeDir,
        data_root_directory: path.join(closeDir, "data"),
        relational_db_url: `sqlite:${closeDb}?mode=rwc`,
      };
    }

    const wal = () => `${closeDb}-wal`;
    const shm = () => `${closeDb}-shm`;

    it("releases the WAL sidecars synchronously", async () => {
      const c = new Cognee(closeSettings());
      await c.warm();
      expect(fs.existsSync(wal())).toBe(true);
      expect(fs.existsSync(shm())).toBe(true);

      c.close();

      // No waiting, no polling.
      expect(fs.existsSync(wal())).toBe(false);
      expect(fs.existsSync(shm())).toBe(false);
    });

    it("is idempotent and rejects ops afterwards", async () => {
      const c = new Cognee(closeSettings());
      await c.warm();
      c.close();
      c.close();

      await expect(c.warm()).rejects.toThrow(/closed/);
      // A failed op must not reopen the database.
      expect(fs.existsSync(wal())).toBe(false);
    });

    it("is a no-op on a handle that was never warmed", () => {
      const c = new Cognee(closeSettings());
      expect(() => c.close()).not.toThrow();
      expect(fs.existsSync(wal())).toBe(false);
    });

    it("is wired to Symbol.dispose so `using` closes the handle", async () => {
      // Exercised without `using` syntax so the test compiles on the repo's
      // ES2020 target; `using` calls exactly this method.
      const c = new Cognee(closeSettings());
      await c.warm();
      expect(fs.existsSync(wal())).toBe(true);
      expect(typeof (c as unknown as Record<symbol, unknown>)[Symbol.dispose]).toBe(
        "function"
      );
      c[Symbol.dispose]();
      expect(fs.existsSync(wal())).toBe(false);
      expect(fs.existsSync(shm())).toBe(false);
    });
  });
});
