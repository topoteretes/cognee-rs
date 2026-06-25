/*
 * cognee-capi — C bindings for the cognee-core pipeline engine.
 */
#ifndef COGNEE_CAPI_H
#define COGNEE_CAPI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    CG_OK = 0, CG_ERR_NULL_POINTER = 1, CG_ERR_INVALID_ARGUMENT = 2,
    CG_ERR_RUNTIME = 3, CG_ERR_TASK_FAILED = 4, CG_ERR_CANCELLED = 5,
    CG_ERR_NO_TASKS = 6, CG_ERR_INVALID_CONFIG = 7, CG_ERR_MISSING_FIELD = 8,
    CG_ERR_TYPE_MISMATCH = 9, CG_ERR_UTF8 = 10,
} CgErrorCode;

const char* cg_last_error_message(void);
void cg_last_error_clear(void);
CgErrorCode cg_init(void);
CgErrorCode cg_init_with_threads(size_t n);
void cg_shutdown(void);
void cg_string_destroy(char* s);

/* Logging (gap-06): argument-less, idempotent.
 * Initializes cognee's logging subsystem from environment variables
 * (COGNEE_LOG_*, LOG_FILE_NAME, LOG_LEVEL, RUST_LOG).
 * Returns 0 on success (or idempotent re-call), non-zero on error. */
int cognee_setup_logging(void);

/* OTLP telemetry (gap-07): argument-less, idempotent.
 * Initializes OpenTelemetry export from environment variables
 * (COGNEE_TRACING_ENABLED, OTEL_EXPORTER_OTLP_ENDPOINT,
 * OTEL_EXPORTER_OTLP_HEADERS, OTEL_SERVICE_NAME and related OTEL_*).
 * If neither COGNEE_TRACING_ENABLED=true nor a non-empty endpoint is
 * set, returns 0 without installing anything.
 * Returns: 0 = success/no-op, 1 = lock poison, 2 = init failed.
 * v1 limitation: the C binding does not install a reload-capable
 * tracing subscriber; only spans emitted via the OpenTelemetry SDK
 * directly reach the collector. See docs/telemetry/07. */
int cognee_init_otlp(void);

/* Product-analytics arming (gap-07 task 06): argument-less, idempotent.
 * Arms cognee product-analytics emission for this process subject to
 * the per-binding policy (decision 11): emission is armed unless
 * TELEMETRY_DISABLED is set, ENV is "test"/"dev", or COGNEE_HOST_SDK
 * is set to any non-empty value. When armed, future calls to
 * cognee_telemetry::env::is_disabled inside the bindings honour the
 * COGNEE_HOST_SDK sentinel (decision 10).
 * Returns: 0 = armed, 1 = not armed (policy suppressed),
 *          2 = lock poison (should not happen).
 * Safe to call multiple times; the first call latches the decision. */
int cognee_init_telemetry(void);

typedef struct CgValue CgValue;
typedef struct CgValueIter CgValueIter;
typedef struct CgTask CgTask;
typedef struct CgTaskInfo CgTaskInfo;
typedef struct CgTaskContext CgTaskContext;
typedef struct CgPipeline CgPipeline;
typedef struct CgPipelineRunResult CgPipelineRunResult;
typedef struct CgPipelineRunHandle CgPipelineRunHandle;
typedef struct CgPipelineWatcher CgPipelineWatcher;
typedef struct CgExecStatusManager CgExecStatusManager;
typedef struct CgCancellationHandle CgCancellationHandle;
typedef struct CgCancellationToken CgCancellationToken;
typedef struct CgProgressToken CgProgressToken;
typedef struct CgRayonThreadPool CgRayonThreadPool;

/* Values */
CgValue* cg_value_from_i64(int64_t v);
CgValue* cg_value_from_f64(double v);
CgValue* cg_value_from_bool(bool v);
CgValue* cg_value_from_string(const char* s);
CgValue* cg_value_from_bytes(const uint8_t* data, size_t len);
CgValue* cg_value_from_opaque(void* data, void (*destructor)(void*));
CgErrorCode cg_value_as_i64(const CgValue* v, int64_t* out);
CgErrorCode cg_value_as_f64(const CgValue* v, double* out);
CgErrorCode cg_value_as_bool(const CgValue* v, bool* out);
CgErrorCode cg_value_as_string(const CgValue* v, const char** out, size_t* len);
CgErrorCode cg_value_as_bytes(const CgValue* v, const uint8_t** out, size_t* len);
CgErrorCode cg_value_as_opaque(const CgValue* v, void** out);
CgValue* cg_value_clone(const CgValue* v);
void cg_value_destroy(CgValue* v);

/* Value Iterator */
typedef struct { CgValue* (*next)(void* state); void (*destroy)(void* state); } CgValueIterVtable;
CgValueIter* cg_value_iter_new(void* state, CgValueIterVtable vtable);
void cg_value_iter_destroy(CgValueIter* iter);

/* Task callback types */
typedef CgErrorCode (*CgSyncFnPtr)(const CgValue*, const CgTaskContext*, void*, CgValue**);
typedef void (*CgAsyncResultCallback)(CgErrorCode, CgValue*, void*);
typedef void (*CgAsyncFnPtr)(const CgValue*, const CgTaskContext*, void*, CgAsyncResultCallback, void*);
typedef CgErrorCode (*CgSyncIterFnPtr)(const CgValue*, const CgTaskContext*, void*, CgValueIter**);
typedef void (*CgStreamYieldFn)(CgValue*, void*);
typedef void (*CgStreamCompleteFn)(CgErrorCode, void*);
typedef void (*CgAsyncStreamFnPtr)(const CgValue*, const CgTaskContext*, void*, CgStreamYieldFn, CgStreamCompleteFn, void*);
typedef CgErrorCode (*CgSyncBatchFnPtr)(const CgValue* const*, size_t, const CgTaskContext*, void*, CgValue**);
typedef void (*CgAsyncBatchFnPtr)(const CgValue* const*, size_t, const CgTaskContext*, void*, CgAsyncResultCallback, void*);
typedef CgErrorCode (*CgSyncIterBatchFnPtr)(const CgValue* const*, size_t, const CgTaskContext*, void*, CgValueIter**);
typedef void (*CgAsyncStreamBatchFnPtr)(const CgValue* const*, size_t, const CgTaskContext*, void*, CgStreamYieldFn, CgStreamCompleteFn, void*);

/* Tasks */
CgTask* cg_task_sync(CgSyncFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_async(CgAsyncFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_sync_iter(CgSyncIterFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_async_stream(CgAsyncStreamFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_sync_batch(CgSyncBatchFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_async_batch(CgAsyncBatchFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_sync_iter_batch(CgSyncIterBatchFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
CgTask* cg_task_async_stream_batch(CgAsyncStreamBatchFnPtr fn_ptr, void* ud, void (*destroy_ud)(void*));
void cg_task_destroy(CgTask* t);

/* TaskInfo */
CgTaskInfo* cg_task_info_new(CgTask* task);
void cg_task_info_set_name(CgTaskInfo* info, const char* name);
void cg_task_info_set_batch_size(CgTaskInfo* info, size_t size);
void cg_task_info_set_weight(CgTaskInfo* info, uint32_t weight);
void cg_task_info_set_summary(CgTaskInfo* info, const char* tmpl);
void cg_task_info_destroy(CgTaskInfo* info);

/* Pipeline */
typedef enum { CG_RETRY_DELAY_CONSTANT = 0, CG_RETRY_DELAY_EXPONENTIAL = 1 } CgRetryDelayKind;
typedef struct { CgRetryDelayKind kind; uint64_t base_ms; uint32_t factor; } CgRetryDelaySpec;

/* Data-ID extraction function pointer for incremental deduplication.
 * Returns true if a data ID was written to buf; *written is set to
 * the number of bytes written (excluding null terminator). */
typedef bool (*CgDataIdFnPtr)(const CgValue* v, char* buf, size_t buf_len,
                               size_t* written, void* user_data);

CgPipeline* cg_pipeline_new(const char* description);
void cg_pipeline_set_name(CgPipeline* p, const char* name);
void cg_pipeline_add_task(CgPipeline* p, CgTaskInfo* info);
void cg_pipeline_set_batch_size(CgPipeline* p, size_t size);
void cg_pipeline_set_concurrency(CgPipeline* p, size_t n);
void cg_pipeline_set_retry_none(CgPipeline* p);
void cg_pipeline_set_retry_limited(CgPipeline* p, uint32_t max_attempts, CgRetryDelaySpec delay);
void cg_pipeline_set_data_id_fn(CgPipeline* p, CgDataIdFnPtr fn_ptr, void* user_data,
                                 void (*destroy_ud)(void*));
void cg_pipeline_destroy(CgPipeline* p);

/* Pipeline execution
 *
 * All three entry points execute the full task list:
 *   cg_pipeline_execute_blocking  — blocks the caller until done.
 *   cg_pipeline_execute_in_background — returns a CgPipelineRunHandle
 *       immediately; wait with cg_run_handle_wait (requires cg_init()).
 *   cg_pipeline_execute_async — invokes callback on completion
 *       (requires cg_init()).
 *
 * The pipeline handle may be destroyed with cg_pipeline_destroy() as
 * soon as the execute call returns — the Arc-shared task list keeps the
 * tasks alive for the duration of the background/async run. */
typedef void (*CgExecutionCallback)(CgErrorCode, CgPipelineRunResult*, void*);
CgErrorCode cg_pipeline_execute_blocking(const CgPipeline*, const CgValue* const*, size_t, const CgTaskContext*, const CgPipelineWatcher*, CgPipelineRunResult**);
CgPipelineRunHandle* cg_pipeline_execute_in_background(const CgPipeline*, const CgValue* const*, size_t, const CgTaskContext*, const CgPipelineWatcher*);
void cg_pipeline_execute_async(const CgPipeline*, const CgValue* const*, size_t, const CgTaskContext*, const CgPipelineWatcher*, CgExecutionCallback, void*);

/* Pipeline run result */
size_t cg_run_result_output_count(const CgPipelineRunResult* r);
CgValue* cg_run_result_output_at(const CgPipelineRunResult* r, size_t index);
void cg_run_result_destroy(CgPipelineRunResult* r);

/* Pipeline run handle */
bool cg_run_handle_is_finished(const CgPipelineRunHandle* h);
void cg_run_handle_abort(CgPipelineRunHandle* h);
void cg_run_handle_wait(CgPipelineRunHandle* h, CgExecutionCallback callback, void* callback_data);
void cg_run_handle_destroy(CgPipelineRunHandle* h);

/* TaskContext */
CgErrorCode cg_task_context_mock(CgCancellationHandle** handle_out, CgTaskContext** ctx_out);
CgTaskContext* cg_task_context_clone(const CgTaskContext* ctx);
void cg_task_context_destroy(CgTaskContext* ctx);

/* Cancellation */
CgErrorCode cg_cancellation_pair(CgCancellationHandle** handle, CgCancellationToken** token);
void cg_cancellation_handle_cancel(CgCancellationHandle* h);
bool cg_cancellation_handle_is_cancelled(const CgCancellationHandle* h);
bool cg_cancellation_token_is_cancelled(const CgCancellationToken* t);
CgCancellationHandle* cg_cancellation_handle_clone(const CgCancellationHandle* h);
CgCancellationToken* cg_cancellation_token_clone(const CgCancellationToken* t);
void cg_cancellation_handle_destroy(CgCancellationHandle* h);
void cg_cancellation_token_destroy(CgCancellationToken* t);

/* Progress */
CgProgressToken* cg_progress_token_new(void);
void cg_progress_token_set(CgProgressToken* t, double fraction);
double cg_progress_token_fraction(const CgProgressToken* t);
double cg_progress_token_width(const CgProgressToken* t);
bool cg_progress_token_is_complete(const CgProgressToken* t);
double cg_progress_token_root_fraction(const CgProgressToken* t);
CgErrorCode cg_progress_token_split(CgProgressToken* t, const uint32_t* weights, size_t count, CgProgressToken*** out, size_t* out_count);
CgProgressToken* cg_progress_token_subtoken(CgProgressToken* t, double frac_width);
CgProgressToken* cg_progress_token_clone(const CgProgressToken* t);
void cg_progress_token_destroy(CgProgressToken* t);
void cg_progress_token_array_destroy(CgProgressToken** arr, size_t count);

/* Thread pool */
CgRayonThreadPool* cg_rayon_thread_pool_new(size_t num_threads);
CgRayonThreadPool* cg_rayon_thread_pool_default(void);
void cg_rayon_thread_pool_destroy(CgRayonThreadPool* pool);

/* Watcher */
typedef struct {
    void (*on_pipeline)(void*, const char*, int, size_t, const char*);
    void (*on_task)(void*, const char*, size_t, const char*, size_t, int, uint32_t, const char*);
    void (*on_run_started)(void*, const char*, const char*);
    void (*on_run_completed)(void*, const char*, size_t);
    void (*on_run_errored)(void*, const char*, const char*);
    void (*on_task_started)(void*, const char*, const char*, size_t);
    void (*on_task_completed)(void*, const char*, const char*, size_t);
    void (*on_task_errored)(void*, const char*, const char*, const char*);
    void (*destroy)(void*);
} CgPipelineWatcherVtable;
CgPipelineWatcher* cg_pipeline_watcher_new(void* state, CgPipelineWatcherVtable vtable);
CgPipelineWatcher* cg_pipeline_watcher_noop(void);
void cg_pipeline_watcher_destroy(CgPipelineWatcher* w);

/* ExecStatusManager */

/**
 * C-side vtable for a custom exec status manager.
 *
 * All callbacks are synchronous.  UUID pointers are 16-byte raw byte arrays
 * in network byte order (big-endian); pass NULL to represent a missing UUID.
 * `state` is forwarded verbatim to every callback and to `destroy`.
 */
typedef struct {
    /** Query whether a data item has already been processed.
     *  Returns true if completed; false otherwise. */
    bool (*is_completed)(
        void*       state,
        const char* data_id,
        const char* pipeline_name,
        const uint8_t* dataset_id   /* 16 bytes or NULL */
    );
    /** Mark a data item as successfully completed. */
    void (*mark_completed)(
        void*       state,
        const char* data_id,
        const char* pipeline_name,
        const uint8_t* dataset_id   /* 16 bytes or NULL */
    );
    /** Mark a data item as failed with an error message. */
    void (*mark_failed)(
        void*       state,
        const char* data_id,
        const char* pipeline_name,
        const uint8_t* dataset_id,  /* 16 bytes or NULL */
        const char* error
    );
    /** Record provenance for a task step. */
    void (*stamp_provenance)(
        void*       state,
        const char* data_id,
        const char* pipeline_name,
        const char* task_name,
        const uint8_t* user_id,     /* 16 bytes or NULL */
        const char* node_set        /* NULL if not set */
    );
    /** Release resources held by the custom manager.
     *  Called exactly once when the manager is destroyed. */
    void (*destroy)(void* state);
} CgExecStatusManagerVtable;

/**
 * Create a custom exec status manager from a C vtable.
 *
 * # Safety
 * `state` must remain valid until `vtable.destroy` is called (i.e. until
 * `cg_exec_status_destroy` is called on the returned pointer).
 * Any callback in `vtable` may be NULL (treated as a no-op / false-return).
 *
 * @param state   Caller-owned opaque pointer forwarded to every vtable call.
 * @param vtable  Vtable struct (copied by value; the caller does not need to
 *                keep it alive after this call returns).
 * @return        Heap-allocated CgExecStatusManager* owned by the caller;
 *                must be freed with cg_exec_status_destroy.
 */
CgExecStatusManager* cg_exec_status_new(void* state, CgExecStatusManagerVtable vtable);

/** Create a no-op exec status manager (processes everything, no dedup). */
CgExecStatusManager* cg_exec_status_noop(void);

/**
 * Destroy an exec status manager.
 *
 * # Safety
 * `mgr` must have been created by this library, or be NULL (no-op).
 */
void cg_exec_status_destroy(CgExecStatusManager* mgr);

#ifdef __cplusplus
}
#endif
#endif /* COGNEE_CAPI_H */
