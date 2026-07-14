package ai.cognee.internal;

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
}
