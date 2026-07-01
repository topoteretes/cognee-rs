// Sources/CogneeSDK/Cognee.swift
import CogneeSDKCore

/// Swift async/await wrapper around the cognee C SDK.
///
/// ## Basic usage
/// ```swift
/// let cognee = try Cognee()
/// try await cognee.warm()
/// let addResult      = try await cognee.add(
///     inputsJSON: #"{"type":"text","text":"Hello, cognee!"}"#,
///     dataset: "demo"
/// )
/// let cognifyResult  = try await cognee.cognify(dataset: "demo")
/// let searchResult   = try await cognee.search(query: "Hello")
/// ```
///
/// ## Thread safety
/// `Cognee` is `@unchecked Sendable` because the underlying `CgSdk` handle
/// is thread-safe (it wraps `Arc<HandleState>` which is `Send+Sync`).
/// Concurrent calls to any method on the same instance are safe.
///
/// ## How async bridging works
/// Every `cg_sdk_*` function takes a `CgSdkResultCallback` function pointer
/// and a `void* user_data` pointer.  The callback fires exactly once on a
/// tokio worker thread (R1 rule, cognee_sdk.h).  We bridge this to Swift
/// async/await using `withCheckedThrowingContinuation`:
///
/// 1. Allocate a `ContinuationBox` on the heap wrapping the Swift continuation.
/// 2. Retain it manually with `Unmanaged.passRetained` and pass the raw pointer
///    as `user_data`.  ARC is bypassed so the box survives until the callback.
/// 3. In the callback (a non-capturing `@convention(c)` closure), recover the
///    box via `Unmanaged.fromOpaque(_:).takeRetainedValue()` and call
///    `continuation.resume(...)` exactly once, balancing the extra retain.
public final class Cognee: @unchecked Sendable {

    // MARK: – Private state

    /// Opaque `CgSdk*` C handle.
    private let handle: OpaquePointer

    // MARK: – Lifecycle

    /// Create a new SDK handle.
    ///
    /// This call is synchronous.  It applies the 3-way settings overlay
    /// (defaults < environment variables < `settingsJSON`).  Network / disk
    /// access does NOT happen here — it happens on `warm()`.
    ///
    /// - Parameter settingsJSON: Optional JSON object whose keys override the
    ///   environment-loaded settings (e.g. `{"llm_api_key":"sk-…"}`).
    ///   Pass `nil` to use environment defaults.
    /// - Throws: `CogneeError` with code 3 (CG_ERR_RUNTIME) if `cg_sdk_new`
    ///   returns `nil`.
    public init(settingsJSON: String? = nil) throws {
        let raw: OpaquePointer? = settingsJSON.map { cg_sdk_new($0) } ?? cg_sdk_new(nil)
        guard let p = raw else {
            let msg = cg_last_error_message().map { String(cString: $0) }
                ?? "cg_sdk_new returned nil"
            throw CogneeError(code: CgErrorCode(rawValue: 3), message: msg)
        }
        handle = p
    }

    deinit {
        // Drops the Arc reference.  In-flight callbacks keep their own Arc
        // clone and may still fire after this point — that is safe by design.
        cg_sdk_destroy(handle)
    }

    // MARK: – SDK ops

    /// Warm the SDK handle: build DB connections, LLM/embedding engine, etc.
    ///
    /// Must be called once before `add`, `cognify`, or `search`.
    /// Calling it again is a no-op (idempotent in the Rust layer).
    public func warm() async throws {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<Void>(cont)).toOpaque()
            cg_sdk_warm(handle, { code, _, message, userData in
                let box = Unmanaged<ContinuationBox<Void>>
                    .fromOpaque(userData!).takeRetainedValue()
                guard code.rawValue == 0 else {
                    box.continuation.resume(
                        throwing: CogneeError(code: code, message: message))
                    return
                }
                box.continuation.resume(returning: ())
            }, ptr)
        }
    }

    /// Return the owner UUID as a quoted JSON string (e.g. `"\"abc-…\""`).
    ///
    /// Warms the handle lazily if services have not yet been built.
    public func ownerId() async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_owner_id(handle, { code, result, message, userData in
                let box = Unmanaged<ContinuationBox<String>>
                    .fromOpaque(userData!).takeRetainedValue()
                guard code.rawValue == 0 else {
                    box.continuation.resume(
                        throwing: CogneeError(code: code, message: message))
                    return
                }
                box.continuation.resume(
                    returning: result.map { String(cString: $0) } ?? "null")
            }, ptr)
        }
    }

    /// Add data to a named dataset.
    ///
    /// - Parameters:
    ///   - inputsJSON: JSON object or array of `CogneeDataInput` values.
    ///     Supported types: `{"type":"text","text":"…"}`, `{"type":"file","path":"…"}`,
    ///     `{"type":"url","url":"…"}`, `{"type":"binary","bytes":"<base64>","name":"…"}`.
    ///   - dataset: Target dataset name.  Auto-created if it does not exist.
    ///   - optsJSON: Optional JSON options (e.g. `{"tenant":"<uuid>"}`).
    /// - Returns: Raw `CogneeAddResult` JSON string.
    public func add(
        inputsJSON: String,
        dataset: String,
        optsJSON: String? = nil
    ) async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_add(handle, inputsJSON, dataset, optsJSON,
                { code, result, message, userData in
                    let box = Unmanaged<ContinuationBox<String>>
                        .fromOpaque(userData!).takeRetainedValue()
                    guard code.rawValue == 0 else {
                        box.continuation.resume(
                            throwing: CogneeError(code: code, message: message))
                        return
                    }
                    box.continuation.resume(
                        returning: result.map { String(cString: $0) } ?? "null")
                }, ptr)
        }
    }

    /// Run the cognify pipeline on an existing dataset.
    ///
    /// The dataset must already exist from a prior `add` call.
    /// This is a long-running operation (seconds to minutes).
    ///
    /// - Parameters:
    ///   - dataset: Dataset name to cognify.
    ///   - optsJSON: Optional JSON options (e.g. `{"chunkSize":512}`).
    /// - Returns: Raw `CogneeCognifyResult` JSON string.
    public func cognify(
        dataset: String,
        optsJSON: String? = nil
    ) async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_cognify(handle, dataset, optsJSON,
                { code, result, message, userData in
                    let box = Unmanaged<ContinuationBox<String>>
                        .fromOpaque(userData!).takeRetainedValue()
                    guard code.rawValue == 0 else {
                        box.continuation.resume(
                            throwing: CogneeError(code: code, message: message))
                        return
                    }
                    box.continuation.resume(
                        returning: result.map { String(cString: $0) } ?? "null")
                }, ptr)
        }
    }

    /// Add data and immediately cognify in one combined operation.
    ///
    /// Only newly-added items are cognified.  If all inputs were duplicates,
    /// cognify is skipped and a zeroed result is returned.
    ///
    /// - Returns: Raw JSON `{"add": CogneeAddResult, "cognify": CogneeCognifyResult}`.
    public func addAndCognify(
        inputsJSON: String,
        dataset: String,
        optsJSON: String? = nil
    ) async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_add_and_cognify(handle, inputsJSON, dataset, optsJSON,
                { code, result, message, userData in
                    let box = Unmanaged<ContinuationBox<String>>
                        .fromOpaque(userData!).takeRetainedValue()
                    guard code.rawValue == 0 else {
                        box.continuation.resume(
                            throwing: CogneeError(code: code, message: message))
                        return
                    }
                    box.continuation.resume(
                        returning: result.map { String(cString: $0) } ?? "null")
                }, ptr)
        }
    }

    /// Search the knowledge graph.
    ///
    /// - Parameters:
    ///   - query: The search query string.
    ///   - optsJSON: Optional JSON options.
    ///     Useful keys: `"searchType"` (e.g. `"GRAPH_COMPLETION"`), `"topK"` (int).
    /// - Returns: Raw `SearchResponse` JSON string (array or object).
    public func search(
        query: String,
        optsJSON: String? = nil
    ) async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_search(handle, query, optsJSON,
                { code, result, message, userData in
                    let box = Unmanaged<ContinuationBox<String>>
                        .fromOpaque(userData!).takeRetainedValue()
                    guard code.rawValue == 0 else {
                        box.continuation.resume(
                            throwing: CogneeError(code: code, message: message))
                        return
                    }
                    box.continuation.resume(
                        returning: result.map { String(cString: $0) } ?? "null")
                }, ptr)
        }
    }

    /// Recall from memory using the unified recall pipeline.
    ///
    /// - Parameters:
    ///   - query: The recall query string.
    ///   - optsJSON: Optional JSON options (e.g. `{"scope":"graph","topK":5}`).
    /// - Returns: Raw `CogneeRecallResult` JSON string.
    public func recall(
        query: String,
        optsJSON: String? = nil
    ) async throws -> String {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            let ptr = Unmanaged.passRetained(ContinuationBox<String>(cont)).toOpaque()
            cg_sdk_recall(handle, query, optsJSON,
                { code, result, message, userData in
                    let box = Unmanaged<ContinuationBox<String>>
                        .fromOpaque(userData!).takeRetainedValue()
                    guard code.rawValue == 0 else {
                        box.continuation.resume(
                            throwing: CogneeError(code: code, message: message))
                        return
                    }
                    box.continuation.resume(
                        returning: result.map { String(cString: $0) } ?? "null")
                }, ptr)
        }
    }

    // MARK: – Configuration

    /// Set a configuration key with a JSON-encoded value.
    ///
    /// Use this for non-string types (booleans, numbers).
    /// Example: `try cognee.configure("llm_mock", jsonValue: "true")`
    ///
    /// This call is synchronous — it does **not** require `warm()` first.
    ///
    /// - Throws: `CogneeError` if the key is unknown or the value is invalid.
    public func configure(_ key: String, jsonValue: String) throws {
        let code = cg_sdk_config_set(handle, key, jsonValue)
        guard code.rawValue == 0 else {
            let msg = cg_last_error_message().map { String(cString: $0) }
                ?? "configure(jsonValue:) failed for key '\(key)'"
            throw CogneeError(code: code, message: msg)
        }
    }

    /// Set a string-typed configuration key.
    ///
    /// Example: `try cognee.configure("llm_cassette", value: "/path/to/cassette.json")`
    ///
    /// This call is synchronous — it does **not** require `warm()` first.
    ///
    /// - Throws: `CogneeError` if the key is unknown or the value is invalid.
    public func configure(_ key: String, value: String) throws {
        let code = cg_sdk_config_set_str(handle, key, value)
        guard code.rawValue == 0 else {
            let msg = cg_last_error_message().map { String(cString: $0) }
                ?? "configure(value:) failed for key '\(key)'"
            throw CogneeError(code: code, message: msg)
        }
    }

    /// Configure the SDK for fully-offline operation using a pre-recorded LLM cassette.
    ///
    /// Enables:
    /// - Mock LLM that replays responses from `cassettePath`
    /// - Deterministic mock embeddings (no embedding model needed)
    /// - In-memory graph store (no database required)
    /// - In-memory brute-force vector store (no vector database required)
    ///
    /// Call this **before** `warm()`.
    ///
    /// - Parameter cassettePath: Filesystem path to a `LlmCassette` JSON file.
    public func configureMockMode(cassettePath: String) throws {
        try configure("llm_mock",                jsonValue: "true")
        try configure("llm_cassette",            value: cassettePath)
        try configure("embedding_provider",      value: "mock")
        try configure("vector_db_provider",      value: "brute-force")
        try configure("graph_database_provider", value: "mock")
    }
}
