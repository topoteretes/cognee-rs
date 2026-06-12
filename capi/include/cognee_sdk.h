/*
 * cognee_sdk.h — C API for the cognee SDK tier.
 *
 * This header exposes the user-facing SDK surface: handle lifecycle,
 * async callback plumbing, and the sync-bridge waiter.  The low-level
 * pipeline engine API lives in cognee.h (do not mix the two tiers).
 *
 * Include this header AFTER cognee.h, or include only this one — it
 * pulls in cognee.h automatically:
 *
 *   #include "cognee_sdk.h"   // also brings in CgErrorCode, cg_string_destroy
 *
 * ## Tier rule (R2)
 *
 * `cg_sdk_*` functions return only:
 *   - CG_OK (0)
 *   - CG_ERR_NULL_POINTER (1)
 *   - CG_ERR_RUNTIME (3)
 *   - CG_ERR_UTF8 (10)
 *   - SDK-tier codes 11–18 (delivered via the callback's `code` param)
 *
 * Engine codes 2 and 4–9 never appear in SDK-tier results.
 *
 * ## Deferred-callback rule (R1)
 *
 * The callback passed to `cg_sdk_warm` / `cg_sdk_owner_id` (and future
 * `cg_sdk_*` ops) is **always** invoked asynchronously — never from inside
 * the initiating `cg_sdk_*` call itself.  This matches the libuv / gRPC /
 * ORT convention and avoids re-entrancy surprises in event loops.
 *
 * ## JSON contract (D3, D9)
 *
 * Every `result_json` pointer in a callback is a valid UTF-8 JSON document:
 *   - void ops:   `"null"`
 *   - UUID:       `"\"<uuid-string>\""`  (quoted JSON string)
 *   - bool ops:   `"true"` or `"false"`
 *   - objects:    `{"camelCaseKey": ...}`
 *   - arrays:     `[...]`
 *
 * Keys are camelCase, byte-identical to the TypeScript SDK wire shapes
 * (js/src/types.ts).  The pointer is valid only inside the callback; copy
 * if you need it afterwards.  Free result strings from `cg_sdk_waiter_wait`
 * with `cg_string_destroy`.
 *
 * ## Ordering footgun (R7)
 *
 * `cg_sdk_new` initialises the global tokio runtime idempotently if it has
 * not yet been started.  If you need a custom worker-thread count, call
 * `cg_init_with_threads(n)` **before** the first `cg_sdk_new` — the
 * OnceLock is already occupied after the first init, so later calls to
 * `cg_init_with_threads` are silently no-ops.
 *
 * ## Cancellation (R4)
 *
 * SDK-op cancellation is an explicit v1 non-goal (TS parity).  A reserved
 * extension shape (optional `CgCancellationToken*` as a trailing parameter)
 * is planned for a future version.
 *
 * ## Thread safety
 *
 * `CgSdk` is thread-safe (it wraps Arc<HandleState> which is Send+Sync).
 * Concurrent calls to `cg_sdk_warm`, `cg_sdk_owner_id`, etc. on the same
 * handle are safe.  `CgSdkWaiter` is single-use and must not be shared
 * across threads (create one per op).
 */
#ifndef COGNEE_SDK_H
#define COGNEE_SDK_H

#include "cognee.h"  /* CgErrorCode, cg_string_destroy */

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── API version ──────────────────────────────────────────────────────────── */

/**
 * Major API version.  Incremented on breaking changes to the SDK C ABI.
 * Phase 1 = 1.
 */
#define CG_API_VERSION_MAJOR 1

/**
 * Minor API version.  Incremented each phase that adds new SDK symbols.
 * Phase 1b = 1 (first SDK symbols); Phase 3 = 2 (config surface);
 * Phase 4 = 3 (add / cognify / add_and_cognify).
 */
#define CG_API_VERSION_MINOR 3

/**
 * Return the packed API version as (major << 16) | minor.
 *
 * Use this at runtime to verify that the loaded shared library matches
 * the header you compiled against:
 *
 *   assert(cg_api_version() == ((CG_API_VERSION_MAJOR << 16) | CG_API_VERSION_MINOR));
 */
uint32_t cg_api_version(void);

/* ── SDK error codes (values 11–18, SDK tier only) ────────────────────────── */
/*
 * These values extend the CgErrorCode enum defined in cognee.h.
 * They appear here as preprocessor constants so C89/C90 consumers can use
 * them without relying on the enum extension.
 *
 * See the tier rule (R2) at the top of this header.
 */
#define CG_ERR_COMPONENT            11
#define CG_ERR_SERVICE_BUILD        12
#define CG_ERR_USER_BOOTSTRAP       13
#define CG_ERR_SDK_VALIDATION       14
#define CG_ERR_UNSUPPORTED          15
#define CG_ERR_FEATURE_NOT_BUILT    16
#define CG_ERR_UNKNOWN_CONFIG_KEY   17
#define CG_ERR_CONFIG_TYPE_MISMATCH 18

/* Mapping to TypeScript SdkError kind strings (js/src/types.ts):
 *   CG_ERR_COMPONENT            ↔ "COMPONENT_ERROR"
 *   CG_ERR_SERVICE_BUILD        ↔ "SERVICE_BUILD_ERROR"
 *   CG_ERR_USER_BOOTSTRAP       ↔ "USER_BOOTSTRAP_ERROR"
 *   (CG_ERR_RUNTIME = 3)        ↔ "RUNTIME_ERROR"
 *   CG_ERR_SDK_VALIDATION       ↔ "VALIDATION_ERROR"
 *   CG_ERR_UNSUPPORTED          ↔ "UNSUPPORTED"
 *   CG_ERR_FEATURE_NOT_BUILT    ↔ "FEATURE_NOT_BUILT"
 *   CG_ERR_UNKNOWN_CONFIG_KEY   ↔ (Phase 3)
 *   CG_ERR_CONFIG_TYPE_MISMATCH ↔ (Phase 3)
 */

/* ── CgSdkResultCallback ──────────────────────────────────────────────────── */

/**
 * Callback invoked exactly once when an async SDK operation completes.
 *
 * @param code          CG_OK on success; an SDK error code (11–18) or one of
 *                      CG_ERR_NULL_POINTER / CG_ERR_RUNTIME / CG_ERR_UTF8
 *                      on failure.
 * @param result_json   On success: a valid JSON document (see JSON contract
 *                      in the file header); NULL on error.
 *                      **Valid only inside the callback** — copy if needed.
 * @param error_message Human-readable error description on failure; NULL on
 *                      success.  **Valid only inside the callback**.
 * @param user_data     The pointer passed to the initiating cg_sdk_* call.
 *
 * The callback fires on a tokio worker thread (R1).  If the calling context
 * requires thread affinity, marshal back yourself.  Do NOT call
 * cg_sdk_waiter_wait from inside a callback — it will deadlock the worker.
 */
typedef void (*CgSdkResultCallback)(
    CgErrorCode     code,
    const char*     result_json,
    const char*     error_message,
    void*           user_data
);

/* ── CgSdkWaiter ─────────────────────────────────────────────────────────── */

/** Opaque single-use sync bridge for async SDK ops. */
typedef struct CgSdkWaiter CgSdkWaiter;

/**
 * Create a new single-use waiter.
 *
 * Usage pattern:
 *
 *   CgSdkWaiter* w = cg_sdk_waiter_new();
 *   cg_sdk_warm(sdk, cg_sdk_waiter_callback, w);
 *   char* result = NULL;
 *   CgErrorCode code = cg_sdk_waiter_wait(w, &result);
 *   // use result ...
 *   cg_string_destroy(result);
 *   cg_sdk_waiter_destroy(w);
 *
 * @return Heap-allocated waiter, or NULL on allocation failure (OOM).
 */
CgSdkWaiter* cg_sdk_waiter_new(void);

/**
 * Pre-built callback for the waiter pattern.
 *
 * Pass this as the `callback` argument and the `CgSdkWaiter*` as `user_data`
 * to any `cg_sdk_*` async op.  The callback stores the result and signals
 * `cg_sdk_waiter_wait` to unblock.
 *
 * @param code          Result code from the completed op.
 * @param result_json   JSON result (valid only inside callback; copied).
 * @param error_message Error message (ignored here; waiter returns the code).
 * @param user_data     Must be a valid `CgSdkWaiter*`.
 */
void cg_sdk_waiter_callback(
    CgErrorCode     code,
    const char*     result_json,
    const char*     error_message,
    void*           user_data
);

/**
 * Block until the associated async op completes.
 *
 * On success (CG_OK), `*out_result_json` is set to a heap-allocated copy of
 * the JSON result string; the caller must free it with `cg_string_destroy`.
 * On error, `*out_result_json` is set to NULL.
 *
 * @param waiter         A waiter created by `cg_sdk_waiter_new`.  Must not
 *                       have been consumed by a previous `wait` call.
 * @param out_result_json  Output parameter; may be NULL if you don't need the
 *                       result string.
 * @return CG_OK on success; CG_ERR_SDK_VALIDATION if the waiter was already
 *         consumed (single-use, R6); CG_ERR_RUNTIME if called from a tokio
 *         worker thread (would deadlock); CG_ERR_NULL_POINTER if `waiter`
 *         is NULL.
 *
 * **Do not call this from inside a CgSdkResultCallback** — it will deadlock.
 */
CgErrorCode cg_sdk_waiter_wait(CgSdkWaiter* waiter, char** out_result_json);

/**
 * Destroy a waiter.  No-op if `waiter` is NULL.
 *
 * Must not be called while `cg_sdk_waiter_wait` is blocking on the same
 * waiter from another thread.
 */
void cg_sdk_waiter_destroy(CgSdkWaiter* waiter);

/* NOTE: cg_sdk_waiter_wait_timeout (timed blocking wait) is an explicit
 * non-goal for v1.  Use cg_sdk_waiter_wait (unbounded) for now; a
 * timeout variant may be added in a future phase without breaking the
 * existing ABI.
 */

/* ── CgSdk handle ─────────────────────────────────────────────────────────── */

/** Opaque SDK handle.  Cheap to share via cg_sdk_clone (Arc inside). */
typedef struct CgSdk CgSdk;

/**
 * Create a new SDK handle.
 *
 * `settings_json` may be NULL (use environment defaults) or a JSON object
 * whose string/numeric keys override the env-loaded settings.  The 3-way
 * overlay (defaults < env < json) is applied synchronously, with no I/O.
 * Network / disk access happens on `cg_sdk_warm`.
 *
 * Example with a JSON override (keys are snake_case Settings field names):
 *
 *   CgSdk* sdk = cg_sdk_new(
 *       "{\"llm_api_key\":\"sk-…\",\"embedding_provider\":\"mock\"}"
 *   );
 *
 * **Ordering footgun (R7)**: if you need a custom worker-thread count, call
 * `cg_init_with_threads(n)` before this function — see the file header.
 *
 * @param settings_json  NULL or a UTF-8 JSON object string.
 * @return Heap-allocated `CgSdk*` on success; NULL on failure (call
 *         `cg_last_error_message()` for the reason).
 */
CgSdk* cg_sdk_new(const char* settings_json);

/**
 * Warm the SDK handle: build and cache the service bundle (DB connections,
 * user bootstrap, embedding/LLM engine init).
 *
 * Async (D4, R1): the callback fires on a tokio worker thread, never from
 * inside this call.  On success `result_json` is `"null"` (D9).
 *
 * In-flight ops keep their own reference to the handle state, so callbacks
 * may fire after `cg_sdk_destroy`.
 *
 * @param sdk       A valid CgSdk*.  NULL → no-op (null-check returns early).
 * @param callback  Called exactly once with the result.
 * @param user_data Forwarded to `callback` unchanged.
 */
void cg_sdk_warm(const CgSdk* sdk, CgSdkResultCallback callback, void* user_data);

/**
 * Return the owner UUID as a quoted JSON string (e.g. `"\"abc…\""`, D9).
 *
 * Warms the handle lazily if services have not yet been built.
 *
 * Async (D4, R1): callback fires on a tokio worker thread.
 *
 * @param sdk       A valid CgSdk*.
 * @param callback  Called exactly once with the quoted UUID or an error.
 * @param user_data Forwarded to `callback` unchanged.
 */
void cg_sdk_owner_id(const CgSdk* sdk, CgSdkResultCallback callback, void* user_data);

/**
 * Arc-clone the handle.  Cheap (one atomic increment).
 *
 * The caller must eventually destroy the returned pointer with
 * `cg_sdk_destroy`.
 *
 * @param sdk  A valid CgSdk*.  NULL → returns NULL.
 * @return     New heap-allocated CgSdk* sharing the same inner state.
 */
CgSdk* cg_sdk_clone(const CgSdk* sdk);

/**
 * Destroy a `CgSdk` handle.
 *
 * Drops the Arc reference.  In-flight async ops keep their own Arc clones,
 * so callbacks registered before this call may still fire afterwards.  Do
 * not access `sdk` from any callback after calling this.
 *
 * No-op if `sdk` is NULL.
 */
void cg_sdk_destroy(CgSdk* sdk);

/* ── Core pipeline ops (Phase 4) ──────────────────────────────────────────── */
/*
 * All three ops are async (D4, R1): the callback fires on a tokio worker
 * thread, never synchronously from the initiating call.
 *
 * Wire shapes (camelCase, D3):
 *
 *   CogneeDataInput  — discriminated union on the "type" field:
 *     {"type":"text",   "text":"…"}
 *     {"type":"file",   "path":"…"}
 *     {"type":"url",    "url":"…"}
 *     {"type":"binary", "bytes":"<base64>", "name":"…"}
 *     "bytes" may also be a JSON number array [0..255].
 *     "s3" and "dataItem" return CG_ERR_UNSUPPORTED (15).
 *
 *   CogneeAddResult — {"datasetName":"…","added":[…],"addedCount":N,
 *                       "deduplicated":[…],"deduplicatedCount":M}
 *
 *   CogneeCognifyResult — {"chunks":N,"entities":N,"edges":N,"summaries":N,
 *                           "embeddings":N,"alreadyCompleted":false,
 *                           "priorPipelineRunId":null}
 *
 * Relative file paths in "file" inputs resolve against the process CWD.
 * Use absolute paths from C for portability.
 *
 * Long-running note: cg_sdk_waiter_wait holds the calling thread for the
 * full duration of cognify (potentially minutes for large datasets).
 * Prefer the async callback directly for UI/event-loop contexts.
 */

/**
 * Add data to the named dataset.
 *
 * inputs_json must be a JSON object (single CogneeDataInput) or a JSON array
 * of CogneeDataInput objects.  dataset_name is the target dataset name; it is
 * auto-created if it does not already exist.  opts_json may be NULL or a JSON
 * object with an optional "tenant" key (UUID string).
 *
 * Deduplication: items that were already present in the dataset are returned
 * in the "deduplicated" array; newly-added items are in "added".
 *
 * Async (D4, R1): callback fires exactly once on a tokio worker thread.
 * result_json on success: CogneeAddResult JSON object.
 *
 * @param sdk          A valid CgSdk*.  NULL → no-op (null-check).
 * @param inputs_json  Null-terminated UTF-8 JSON string (object or array).
 * @param dataset_name Null-terminated UTF-8 dataset name.
 * @param opts_json    NULL or null-terminated UTF-8 JSON options object.
 * @param callback     Called exactly once with the result.
 * @param user_data    Forwarded to callback unchanged.
 */
void cg_sdk_add(const CgSdk*        sdk,
                const char*         inputs_json,
                const char*         dataset_name,
                const char*         opts_json,
                CgSdkResultCallback callback,
                void*               user_data);

/**
 * Run the cognify pipeline on an existing dataset.
 *
 * The dataset must already exist (created by a prior cg_sdk_add call).
 * opts_json may be NULL or a JSON object with optional keys:
 *   "tenant"         — UUID string
 *   "chunkSize"      — integer
 *   "chunkOverlap"   — integer
 *   "summarization"  — boolean
 *   "temporalCognify"— boolean
 *   "triplet"        — boolean
 *
 * Async (D4, R1): callback fires exactly once on a tokio worker thread.
 * result_json on success: CogneeCognifyResult JSON object.
 *
 * @param sdk          A valid CgSdk*.
 * @param dataset_name Null-terminated UTF-8 dataset name.
 * @param opts_json    NULL or null-terminated UTF-8 JSON options object.
 * @param callback     Called exactly once with the result.
 * @param user_data    Forwarded to callback unchanged.
 */
void cg_sdk_cognify(const CgSdk*        sdk,
                    const char*         dataset_name,
                    const char*         opts_json,
                    CgSdkResultCallback callback,
                    void*               user_data);

/**
 * Add data and immediately cognify — a single combined op.
 *
 * Equivalent to cg_sdk_add + cg_sdk_cognify on the same dataset, but cognify
 * only processes the **newly-added** items.  If all inputs were duplicates,
 * cognify is skipped and a zeroed CogneeCognifyResult is returned.
 *
 * Async (D4, R1): callback fires exactly once on a tokio worker thread.
 * result_json on success:
 *   {"add": CogneeAddResult, "cognify": CogneeCognifyResult}
 *
 * @param sdk          A valid CgSdk*.
 * @param inputs_json  Null-terminated UTF-8 JSON string (object or array).
 * @param dataset_name Null-terminated UTF-8 dataset name.
 * @param opts_json    NULL or null-terminated UTF-8 JSON options object.
 * @param callback     Called exactly once with the result.
 * @param user_data    Forwarded to callback unchanged.
 */
void cg_sdk_add_and_cognify(const CgSdk*        sdk,
                             const char*         inputs_json,
                             const char*         dataset_name,
                             const char*         opts_json,
                             CgSdkResultCallback callback,
                             void*               user_data);

/* ── Config surface (Phase 3, D7) ─────────────────────────────────────────── */
/*
 * All config functions are SYNCHRONOUS (config mutation is in-memory only).
 * They do NOT use the CgSdkResultCallback / CgSdkWaiter pattern.
 *
 * Every successful call increments the config version.  The next call to
 * cg_sdk_warm (or any service-requiring op) detects the version advance and
 * rebuilds the engine bundle — so config changes take effect on the next warm.
 *
 * Key names are the Rust Settings field names (snake_case), e.g.:
 *   LLM:        llm_provider  llm_model  llm_api_key  llm_endpoint
 *               llm_api_version  llm_temperature  llm_streaming
 *               llm_max_completion_tokens  llm_max_retries  llm_max_parallel_requests
 *   Embedding:  embedding_provider  embedding_model  embedding_dimensions
 *               embedding_endpoint  embedding_api_key  embedding_model_path
 *               embedding_tokenizer_path
 *   Vector DB:  vector_db_provider  vector_db_url  vector_db_key
 *               vector_db_host  vector_db_port  vector_db_name
 *   Graph DB:   graph_database_provider  graph_model  graph_file_path
 *   Chunking:   chunk_strategy  chunk_engine  chunk_size  chunk_overlap
 *   Paths:      system_root_directory  data_root_directory
 *               cache_root_directory  logs_root_directory
 *   Ontology:   ontology_file_path  ontology_resolver  ontology_matching_strategy
 *   Misc:       monitoring_tool  classification_model  summarization_model
 *
 * Error codes (R2):
 *   CG_ERR_UNKNOWN_CONFIG_KEY   (17) — unrecognised key
 *   CG_ERR_CONFIG_TYPE_MISMATCH (18) — JSON type does not match the field type
 *   CG_ERR_SDK_VALIDATION       (14) — malformed JSON
 *   CG_ERR_NULL_POINTER          (1) — null sdk / key / value pointer
 *   CG_ERR_UTF8                 (10) — non-UTF-8 input string
 */

/**
 * Set a single configuration key to a JSON-encoded value.
 *
 * `key` is a Settings field name (snake_case).
 * `value_json` is a valid JSON value:
 *   - string fields: "\"openai\""  (a JSON string literal, quotes included)
 *   - numeric fields: "0.7", "4096"
 *   - boolean fields: "true" or "false"
 *
 * @param sdk         A valid CgSdk*.  NULL → CG_ERR_NULL_POINTER.
 * @param key         Config key (snake_case, null-terminated UTF-8).
 * @param value_json  JSON-encoded value (null-terminated UTF-8).
 * @return CG_OK on success; an error code otherwise (see above).
 */
CgErrorCode cg_sdk_config_set(const CgSdk* sdk,
                               const char* key,
                               const char* value_json);

/**
 * Set a string-typed configuration key from a plain C string (convenience).
 *
 * Equivalent to cg_sdk_config_set with value_json = "\"<value>\"".
 * Covers ~80% of keys without requiring the caller to JSON-escape the value.
 *
 * @param sdk    A valid CgSdk*.  NULL → CG_ERR_NULL_POINTER.
 * @param key    Config key (snake_case, null-terminated UTF-8).
 * @param value  Plain string value (null-terminated UTF-8, no JSON escaping).
 * @return CG_OK on success; an error code otherwise (see above).
 */
CgErrorCode cg_sdk_config_set_str(const CgSdk* sdk,
                                   const char* key,
                                   const char* value);

/**
 * Bulk-update LLM configuration from a JSON object.
 *
 * `llm_config_json` must be a JSON object with LLM keys as documented above.
 * Unknown keys return CG_ERR_UNKNOWN_CONFIG_KEY.
 *
 * @param sdk             A valid CgSdk*.
 * @param llm_config_json JSON object string (null-terminated UTF-8).
 * @return CG_OK on success; an error code otherwise.
 */
CgErrorCode cg_sdk_config_set_llm_config(const CgSdk* sdk,
                                          const char* llm_config_json);

/**
 * Bulk-update embedding configuration from a JSON object.
 *
 * @param sdk                  A valid CgSdk*.
 * @param embedding_config_json JSON object string (null-terminated UTF-8).
 * @return CG_OK on success; an error code otherwise.
 */
CgErrorCode cg_sdk_config_set_embedding_config(const CgSdk* sdk,
                                                const char* embedding_config_json);

/**
 * Bulk-update vector DB configuration from a JSON object.
 *
 * @param sdk                  A valid CgSdk*.
 * @param vector_db_config_json JSON object string (null-terminated UTF-8).
 * @return CG_OK on success; an error code otherwise.
 */
CgErrorCode cg_sdk_config_set_vector_db_config(const CgSdk* sdk,
                                                const char* vector_db_config_json);

/**
 * Bulk-update graph DB configuration from a JSON object.
 *
 * @param sdk                 A valid CgSdk*.
 * @param graph_db_config_json JSON object string (null-terminated UTF-8).
 * @return CG_OK on success; an error code otherwise.
 */
CgErrorCode cg_sdk_config_set_graph_db_config(const CgSdk* sdk,
                                               const char* graph_db_config_json);

/**
 * Read back the current configuration as a JSON string.
 *
 * Secret fields are replaced with "***REDACTED***" before serialization.
 * The redacted fields are: llm_api_key, embedding_api_key, vector_db_key,
 * vector_db_password, graph_database_key, graph_database_password,
 * db_password, cache_password, default_user_password,
 * otel_exporter_otlp_headers.
 *
 * On success `*out_json` is set to a heap-allocated UTF-8 JSON string.
 * The caller must free it with `cg_string_destroy`.
 *
 * @param sdk       A valid CgSdk*.  NULL → CG_ERR_NULL_POINTER.
 * @param out_json  Output: set to the heap-allocated JSON string on CG_OK,
 *                  or NULL on error.  Must not be NULL itself.
 * @return CG_OK on success; CG_ERR_NULL_POINTER, CG_ERR_RUNTIME on failure.
 */
CgErrorCode cg_sdk_config_get(const CgSdk* sdk, char** out_json);

#ifdef __cplusplus
}
#endif
#endif /* COGNEE_SDK_H */
