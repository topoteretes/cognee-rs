/**
 * Typed JS error classes for the cognee SDK.
 *
 * Neon 1.x cannot throw custom Error subclass instances from Rust — only plain
 * `Error` objects with extra properties. The TS layer re-wraps the thrown
 * native error into the correct subclass via `wrapNativeError`.
 *
 * Both `kind` and `code` carry the same string value. `kind` is the stable API
 * identifier; `code` is kept as a backwards-compatible alias so existing
 * call-sites that check `e.code` continue to work.
 */

/** Base class for all cognee SDK errors. */
export class CogneeError extends Error {
  readonly kind: string;
  readonly code: string;

  constructor(message: string, kind: string) {
    super(message);
    this.name = "CogneeError";
    this.kind = kind;
    this.code = kind; // alias for backwards compat
    // Restore prototype chain for instanceof checks across transpiler targets.
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** A component (storage / database / graph / vector / embedding / llm) failed to initialise. */
export class ComponentError extends CogneeError {
  constructor(message: string) {
    super(message, "COMPONENT_ERROR");
    this.name = "ComponentError";
  }
}

/** A derived service (thread pool, session store, ontology resolver, …) failed to construct. */
export class ServiceBuildError extends CogneeError {
  constructor(message: string) {
    super(message, "SERVICE_BUILD_ERROR");
    this.name = "ServiceBuildError";
  }
}

/** The relational user bootstrap (`get_or_create_default_user`) failed. */
export class UserBootstrapError extends CogneeError {
  constructor(message: string) {
    super(message, "USER_BOOTSTRAP_ERROR");
    this.name = "UserBootstrapError";
  }
}

/** A runtime / infrastructure failure (e.g. building the tokio runtime). */
export class RuntimeError extends CogneeError {
  constructor(message: string) {
    super(message, "RUNTIME_ERROR");
    this.name = "RuntimeError";
  }
}

/** Invalid input from the JS boundary (bad shape / missing field / parse failure). */
export class ValidationError extends CogneeError {
  constructor(message: string) {
    super(message, "VALIDATION_ERROR");
    this.name = "ValidationError";
  }
}

/** A requested input variant or feature is recognised but not yet wired end-to-end. */
export class UnsupportedError extends CogneeError {
  constructor(message: string) {
    super(message, "UNSUPPORTED");
    this.name = "UnsupportedError";
  }
}

/** The native function was called but the required Cargo feature was not compiled in. */
export class FeatureNotBuiltError extends CogneeError {
  constructor(message: string) {
    super(message, "FEATURE_NOT_BUILT");
    this.name = "FeatureNotBuiltError";
  }
}

/** An unknown config key was passed to `configSet` / a bulk setter. */
export class UnknownConfigKeyError extends CogneeError {
  constructor(message: string) {
    super(message, "UNKNOWN_CONFIG_KEY");
    this.name = "UnknownConfigKeyError";
  }
}

/** A config value had the wrong type (e.g. a string where a number was expected). */
export class ConfigTypeMismatchError extends CogneeError {
  constructor(message: string) {
    super(message, "CONFIG_TYPE_MISMATCH");
    this.name = "ConfigTypeMismatchError";
  }
}

/** Map from `kind` string to the concrete error constructor. */
const KIND_TO_CLASS: Record<string, new (msg: string) => CogneeError> = {
  COMPONENT_ERROR: ComponentError,
  SERVICE_BUILD_ERROR: ServiceBuildError,
  USER_BOOTSTRAP_ERROR: UserBootstrapError,
  RUNTIME_ERROR: RuntimeError,
  VALIDATION_ERROR: ValidationError,
  UNSUPPORTED: UnsupportedError,
  FEATURE_NOT_BUILT: FeatureNotBuiltError,
  UNKNOWN_CONFIG_KEY: UnknownConfigKeyError,
  CONFIG_TYPE_MISMATCH: ConfigTypeMismatchError,
};

/**
 * Re-wrap a native thrown error (plain `Error` with `kind`/`code` properties)
 * into the correct `CogneeError` subclass.
 *
 * Usage in the Phase-7 `Cognee` class wrapper:
 * ```ts
 * try {
 *   return await native.cogneeAdd(...);
 * } catch (e) {
 *   throw wrapNativeError(e);
 * }
 * ```
 */
export function wrapNativeError(e: unknown): CogneeError {
  if (e instanceof CogneeError) return e;

  if (e != null && typeof e === "object") {
    const obj = e as Record<string, unknown>;
    // Prefer `kind`; fall back to `code` for legacy native errors that
    // may have been thrown before the `kind` field was added.
    const kind =
      typeof obj["kind"] === "string"
        ? obj["kind"]
        : typeof obj["code"] === "string"
          ? obj["code"]
          : "RUNTIME_ERROR";
    const msg =
      typeof obj["message"] === "string" ? obj["message"] : String(e);
    const Cls = KIND_TO_CLASS[kind];
    const wrapped =
      Cls != null
        ? new Cls(msg)
        : // Unknown kind: construct the base class directly so kind/code are set.
          new CogneeError(msg, kind);
    // Preserve the original native stack trace if available.
    if (typeof obj["stack"] === "string") {
      wrapped.stack = obj["stack"];
    }
    return wrapped;
  }

  return new CogneeError(String(e), "RUNTIME_ERROR");
}
