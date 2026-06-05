/**
 * Phase 8 — Tier-A error marshalling tests.
 *
 * Asserts that thrown JS errors carry both `code` and `kind` properties with
 * the same value, confirming the Phase-8 error contract.
 *
 * All tests run WITHOUT LLM / network / embedding models — they exercise only
 * the input-validation layer and config-mutation layer that can throw
 * synchronously or before any async I/O is needed.
 *
 * Env used: MOCK_EMBEDDING=true (set inline), dummy llm_api_key, temp dir.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native } from "../src/native";

// ─── helpers ──────────────────────────────────────────────────────────────

/** Create a minimal CogneeHandle wired against a temp dir (no LLM needed). */
function makeTempHandle(tmpDir: string) {
  return native.cogneeNew({
    system_root_directory: tmpDir,
    data_root_directory: path.join(tmpDir, "data"),
    embedding_provider: "mock",
    llm_api_key: "test-dummy-key",
  });
}

// ─── suite ────────────────────────────────────────────────────────────────

describe("Phase-8 error marshalling (Tier-A)", () => {
  let tmpDir: string;

  beforeAll(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-errors-test-"));
    // Ensure mock embedding is always used; harmless if already set.
    process.env["MOCK_EMBEDDING"] = "true";
  });

  afterAll(() => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best effort cleanup
    }
  });

  // ── UNKNOWN_CONFIG_KEY ──────────────────────────────────────────────────

  it("configSet with unknown key throws code=UNKNOWN_CONFIG_KEY and kind=UNKNOWN_CONFIG_KEY", () => {
    const handle = makeTempHandle(tmpDir);
    let caught: unknown;
    try {
      native.configSet(handle, "nonexistent_key", "some_value");
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeDefined();
    const err = caught as { message?: string; code?: string; kind?: string };
    expect(err.message).toBeDefined();
    expect(err.code).toBe("UNKNOWN_CONFIG_KEY");
    expect(err.kind).toBe("UNKNOWN_CONFIG_KEY");
    // Both must carry the same value (kind is the stable alias, code is legacy).
    expect(err.kind).toBe(err.code);
  });

  // ── CONFIG_TYPE_MISMATCH ────────────────────────────────────────────────

  it("configSet with wrong type throws code=CONFIG_TYPE_MISMATCH and kind=CONFIG_TYPE_MISMATCH", () => {
    const handle = makeTempHandle(tmpDir);
    let caught: unknown;
    try {
      // chunk_size is a u32; passing a non-numeric string must throw TypeMismatch.
      native.configSet(handle, "chunk_size", "not-a-number");
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeDefined();
    const err = caught as { message?: string; code?: string; kind?: string };
    expect(err.message).toBeDefined();
    expect(err.code).toBe("CONFIG_TYPE_MISMATCH");
    expect(err.kind).toBe("CONFIG_TYPE_MISMATCH");
    expect(err.kind).toBe(err.code);
  });

  // ── VALIDATION_ERROR from cogneeAdd ────────────────────────────────────
  //
  // `marshal_inputs` runs before `state.services()` in `run_add`, so an
  // invalid input type triggers VALIDATION_ERROR without booting any
  // LLM/embedding/db infrastructure.

  it("cogneeAdd with an unknown input type promise-rejects with code=VALIDATION_ERROR and kind=VALIDATION_ERROR", async () => {
    const handle = makeTempHandle(tmpDir);
    let caught: unknown;
    try {
      // `{ type: "not_a_real_type" }` is an unknown discriminant — Rust
      // returns SdkError::Validation before touching the services.
      await native.cogneeAdd(
        handle,
        { type: "not_a_real_type" } as unknown as Parameters<
          typeof native.cogneeAdd
        >[1],
        "test_ds",
      );
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeDefined();
    const err = caught as { message?: string; code?: string; kind?: string };
    expect(err.message).toBeDefined();
    expect(err.code).toBe("VALIDATION_ERROR");
    expect(err.kind).toBe("VALIDATION_ERROR");
    expect(err.kind).toBe(err.code);
  });

  // ── wrapNativeError ─────────────────────────────────────────────────────

  it("wrapNativeError returns a CogneeError subclass with the right kind", async () => {
    const { wrapNativeError, UnknownConfigKeyError, ValidationError, CogneeError } =
      await import("../src/errors");

    // Simulate a plain Error thrown by the native layer with code + kind.
    const native_err = Object.assign(new Error("unknown key: foo"), {
      code: "UNKNOWN_CONFIG_KEY",
      kind: "UNKNOWN_CONFIG_KEY",
    });

    const wrapped = wrapNativeError(native_err);
    expect(wrapped).toBeInstanceOf(CogneeError);
    expect(wrapped).toBeInstanceOf(UnknownConfigKeyError);
    expect(wrapped.kind).toBe("UNKNOWN_CONFIG_KEY");
    expect(wrapped.code).toBe("UNKNOWN_CONFIG_KEY");
    expect(wrapped.message).toBe("unknown key: foo");
  });

  it("wrapNativeError falls back to code when kind is absent", async () => {
    const { wrapNativeError, ValidationError } = await import("../src/errors");

    const legacy_err = Object.assign(new Error("validation error: bad"), {
      code: "VALIDATION_ERROR",
      // no `kind` property — legacy error shape before Phase 8
    });

    const wrapped = wrapNativeError(legacy_err);
    expect(wrapped).toBeInstanceOf(ValidationError);
    expect(wrapped.kind).toBe("VALIDATION_ERROR");
    expect(wrapped.code).toBe("VALIDATION_ERROR");
  });

  it("wrapNativeError wraps an unknown kind into CogneeError", async () => {
    const { wrapNativeError, CogneeError } = await import("../src/errors");

    const unknown_err = Object.assign(new Error("mystery"), {
      code: "MYSTERY_CODE",
      kind: "MYSTERY_CODE",
    });

    const wrapped = wrapNativeError(unknown_err);
    expect(wrapped).toBeInstanceOf(CogneeError);
    expect(wrapped.kind).toBe("MYSTERY_CODE");
    expect(wrapped.code).toBe("MYSTERY_CODE");
  });

  it("wrapNativeError passes through an existing CogneeError unchanged", async () => {
    const { wrapNativeError, RuntimeError } = await import("../src/errors");

    const already_wrapped = new RuntimeError("already a CogneeError");
    const result = wrapNativeError(already_wrapped);
    expect(result).toBe(already_wrapped); // exact same reference
  });
});
