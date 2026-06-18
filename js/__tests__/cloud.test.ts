/**
 * Cloud ops — serve / disconnect.
 *
 * Only **direct mode** is tested here (pass `url` + `apiKey` to skip the
 * Auth0 device-code flow, which requires a TTY and is not testable
 * non-interactively).  Both the cloud feature flag case and the direct-mode
 * case are exercised without needing a running Cognee Cloud instance.
 *
 * The Auth0 flow (`cogneeServe({})` with no `url`) is intentionally omitted —
 * it blocks on a device-code prompt and cannot be driven headlessly.
 *
 * These tests require no LLM / no embedding model.
 *
 * Note: both `cogneeServe` and `cogneeDisconnect` throw a typed error with
 * `code = "FEATURE_NOT_BUILT"` when the `cloud` Cargo feature was not compiled
 * in.  That case is handled here so the suite still passes on builds without
 * the cloud feature.
 *
 * The runtime must be initialised before calling module-level cloud ops
 * (`native.init()` in beforeAll).
 */
import { native } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function errorCode(err: unknown): string | null {
  if (err && typeof err === "object" && "code" in err) {
    return (err as { code: string }).code;
  }
  return null;
}

// ─── suite ────────────────────────────────────────────────────────────────────

describe("cloud ops (Tier-A — no LLM)", () => {
  beforeAll(() => {
    // Cloud ops are module-level (no Cognee handle), so the Tokio runtime
    // must be initialised explicitly here.
    native.init();
  });

  // ── disconnect — always runs (no server needed, mirrors Python test) ───────
  //
  // disconnect() when not connected should be a no-op or return undefined.
  // With FEATURE_NOT_BUILT both ops throw immediately.

  it("cogneeDisconnect completes or throws FEATURE_NOT_BUILT when not connected", async () => {
    let caught: unknown;
    try {
      await native.cogneeDisconnect();
    } catch (err) {
      caught = err;
    }
    if (caught !== undefined) {
      const code = errorCode(caught);
      // Only FEATURE_NOT_BUILT or RUNTIME_ERROR are acceptable typed errors.
      expect(["RUNTIME_ERROR", "FEATURE_NOT_BUILT"]).toContain(code);
    }
    // No error thrown → disconnect was a no-op (already disconnected / not connected).
  }, 10_000);

  it("cogneeDisconnect with wipeCredentials=false completes or throws FEATURE_NOT_BUILT", async () => {
    let caught: unknown;
    try {
      await native.cogneeDisconnect({ wipeCredentials: false });
    } catch (err) {
      caught = err;
    }
    if (caught !== undefined) {
      const code = errorCode(caught);
      expect(["RUNTIME_ERROR", "FEATURE_NOT_BUILT"]).toContain(code);
    }
  }, 10_000);

  it("cogneeDisconnect with wipeCredentials=true completes or throws FEATURE_NOT_BUILT", async () => {
    let caught: unknown;
    try {
      await native.cogneeDisconnect({ wipeCredentials: true });
    } catch (err) {
      caught = err;
    }
    if (caught !== undefined) {
      const code = errorCode(caught);
      expect(["RUNTIME_ERROR", "FEATURE_NOT_BUILT"]).toContain(code);
    }
  }, 10_000);

  // ── direct mode ──────────────────────────────────────────────────────────
  //
  // When the cloud feature IS compiled in, direct-mode serve stores the URL and
  // returns {connected: true} without actually verifying connectivity (the
  // first real API call would fail).  When the feature is NOT compiled in,
  // both ops throw FEATURE_NOT_BUILT.
  //
  // Either outcome is acceptable in CI.

  it("cogneeServe in direct mode with a dummy URL returns a ServeResult or throws FEATURE_NOT_BUILT", async () => {
    let result: { connected?: boolean; serviceUrl?: string } | undefined;
    let caught: unknown;
    try {
      result = await native.cogneeServe({
        url: "http://localhost:19999", // dummy — direct mode stores, does not verify
        apiKey: "test-dummy-api-key",
      });
    } catch (err) {
      caught = err;
    }

    if (caught !== undefined) {
      // Feature not built — only FEATURE_NOT_BUILT is acceptable.
      expect(errorCode(caught)).toBe("FEATURE_NOT_BUILT");
    } else {
      // Feature present — result should be a ServeResult object.
      expect(typeof result).toBe("object");
    }
  }, 15_000);

  // ── serve with a real server (credential-gated) ───────────────────────────
  //
  // Mirrors the Python test_serve_direct_mode: only runs when
  // COGNEE_TEST_SERVER_URL is set.

  it("cogneeServe in direct mode returns connected=true when COGNEE_TEST_SERVER_URL is set", async () => {
    const serverUrl = process.env.COGNEE_TEST_SERVER_URL;
    if (!serverUrl) {
      // Gracefully skip: print a message and pass.
      console.log(
        "SKIP: COGNEE_TEST_SERVER_URL not set — skipping direct-mode serve test"
      );
      return;
    }
    const result = await native.cogneeServe({ url: serverUrl, apiKey: "" });
    expect(typeof result).toBe("object");
    expect((result as { connected?: boolean }).connected).toBe(true);
  }, 15_000);
});
