package ai.cognee.internal;

import java.util.concurrent.CompletableFuture;

/**
 * Package-private 1:1 mirror of the Rust {@code Java_ai_cognee_internal_Native_*}
 * exports. Not part of the public API; excluded from published Javadoc.
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
    static native String version();

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
}
