package ai.cognee.internal;

import java.util.concurrent.CompletableFuture;

/**
 * Internal 1:1 mirror of the Rust {@code Java_ai_cognee_internal_Native_*}
 * exports. This is an internal API with no compatibility guarantees — it is not
 * part of the public API and is excluded from published Javadoc. Although the
 * class is {@code public} (JNI requires the native methods to be reachable from
 * an exported package), it must not be used directly.
 */
public final class Native {
    static {
        NativeLibLoader.load();
        String jar = NativeLibLoader.jarVersion();
        String nat = version();
        if (!jar.equals(nat)) {
            throw new IllegalStateException(
                    "cognee native/jar version skew: jar=" + jar + " native=" + nat
                            + " — the bundled native library does not match this jar.");
        }
    }

    private Native() {}

    /** The Rust crate version (from {@code CARGO_PKG_VERSION}). */
    public static native String version();

    /** Create a native handle from a settings JSON string (or null for env). */
    public static native long newHandle(String settingsJson);

    /** Free a native handle. Safe with 0; called at most once per handle. */
    public static native void destroy(long handle);

    public static native void configSet(long handle, String key, String valueJson);

    public static native void configSetLlmConfig(long handle, String mapJson);

    public static native void configSetEmbeddingConfig(long handle, String mapJson);

    public static native void configSetVectorDbConfig(long handle, String mapJson);

    public static native void configSetGraphDbConfig(long handle, String mapJson);

    public static native String getConfig(long handle);

    public static native void warm(long handle, CompletableFuture<String> future);

    public static native void ownerId(long handle, CompletableFuture<String> future);

    public static native void add(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);

    public static native void cognify(long handle, String datasetName, String optsJson,
            CompletableFuture<String> future);

    public static native void addAndCognify(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);

    public static native void search(long handle, String query, String optsJson,
            CompletableFuture<String> future);

    public static native void recall(long handle, String query, String optsJson,
            CompletableFuture<String> future);

    public static native void remember(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);

    public static native void rememberEntry(long handle, String entryJson, String datasetName,
            String sessionId, String optsJson, CompletableFuture<String> future);

    public static native void memify(long handle, String optsJson, CompletableFuture<String> future);

    public static native void improve(long handle, String optsJson, CompletableFuture<String> future);

    // data ops
    public static native void forget(long handle, String targetJson, String optsJson,
            CompletableFuture<String> future);
    public static native void update(long handle, String dataId, String newDataJson,
            String datasetName, String optsJson, CompletableFuture<String> future);
    public static native void pruneData(long handle, CompletableFuture<String> future);
    public static native void pruneSystem(long handle, String optsJson,
            CompletableFuture<String> future);

    // dataset ops
    public static native void listDatasets(long handle, CompletableFuture<String> future);
    public static native void listData(long handle, String datasetId, CompletableFuture<String> future);
    public static native void hasData(long handle, String datasetId, CompletableFuture<String> future);
    public static native void datasetStatus(long handle, String datasetIdsJson,
            CompletableFuture<String> future);
    public static native void emptyDataset(long handle, String datasetId,
            CompletableFuture<String> future);
    public static native void deleteData(long handle, String datasetId, String dataId,
            String optsJson, CompletableFuture<String> future);
    public static native void deleteAllDatasets(long handle, CompletableFuture<String> future);

    // sessions
    public static native void getSession(long handle, String sessionId, String optsJson,
            CompletableFuture<String> future);
    public static native void addFeedback(long handle, String sessionId, String qaId,
            String optsJson, CompletableFuture<String> future);
    public static native void deleteFeedback(long handle, String sessionId, String qaId,
            CompletableFuture<String> future);
    public static native void getGraphContext(long handle, String sessionId,
            CompletableFuture<String> future);
    public static native void setGraphContext(long handle, String sessionId, String context,
            CompletableFuture<String> future);
    // users / admin
    public static native void getOrCreateDefaultUser(long handle, CompletableFuture<String> future);
    public static native void resetPipelineRunStatus(long handle, String datasetId,
            String pipelineName, CompletableFuture<String> future);
    public static native void resetDatasetPipelineRunStatus(long handle, String datasetId,
            CompletableFuture<String> future);
    // notebooks
    public static native void listNotebooks(long handle, CompletableFuture<String> future);
    public static native void createNotebook(long handle, String name, String cellsJson,
            boolean deletable, CompletableFuture<String> future);
    public static native void updateNotebook(long handle, String id, String patchJson,
            CompletableFuture<String> future);
    public static native void deleteNotebook(long handle, String id,
            CompletableFuture<String> future);

    // visualization
    public static native void visualize(long handle, String optsJson,
            CompletableFuture<String> future);
    public static native void visualizeToFile(long handle, String optsJson,
            CompletableFuture<String> future);
    // module-level statics
    public static native void setupLogging();
    public static native void initOtlp();
    public static native boolean initTelemetry();
}
