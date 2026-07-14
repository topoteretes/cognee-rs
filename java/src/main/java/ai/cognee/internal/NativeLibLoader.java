package ai.cognee.internal;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;
import java.util.Properties;

/** Loads the {@code cognee_java} native library and exposes the jar version. */
final class NativeLibLoader {
    private static volatile boolean loaded = false;

    private NativeLibLoader() {}

    /** Load the native library exactly once. */
    static synchronized void load() {
        if (loaded) {
            return;
        }
        String override = System.getenv("COGNEE_JAVA_LIB_PATH");
        if (override != null && !override.isEmpty()) {
            System.load(override);
        } else {
            extractAndLoadFromJar();
        }
        loaded = true;
    }

    /** The jar's Maven version, read from the filtered classpath resource. */
    static String jarVersion() {
        try (InputStream in =
                NativeLibLoader.class.getResourceAsStream("/ai/cognee/version.properties")) {
            if (in == null) {
                throw new IllegalStateException("version.properties missing from jar");
            }
            Properties p = new Properties();
            p.load(in);
            String v = p.getProperty("version");
            if (v == null || v.isEmpty() || v.contains("${")) {
                throw new IllegalStateException("version.properties not filtered: " + v);
            }
            return v;
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    private static void extractAndLoadFromJar() {
        String classifier = platformClassifier();
        String libFile = libFileName();
        String resource = "/native/" + classifier + "/" + libFile;
        try (InputStream in = NativeLibLoader.class.getResourceAsStream(resource)) {
            if (in == null) {
                throw new UnsatisfiedLinkError(
                        "no bundled native library for platform '" + classifier
                                + "' (resource " + resource + "). Set COGNEE_JAVA_LIB_PATH"
                                + " to a locally built cdylib for development.");
            }
            Path tmp = Files.createTempFile("cognee_java", suffix());
            tmp.toFile().deleteOnExit();
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            System.load(tmp.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    /** OpenDAL/RocksDB-style classifier: {os}-{arch}. */
    private static String platformClassifier() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        boolean aarch64 = arch.contains("aarch64") || arch.contains("arm64");
        if (os.contains("linux")) {
            return aarch64 ? "linux-aarch_64" : "linux-x86_64";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return "osx-aarch_64";
        }
        if (os.contains("win")) {
            return "windows-x86_64";
        }
        throw new UnsatisfiedLinkError("unsupported platform: os=" + os + " arch=" + arch);
    }

    private static String libFileName() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (os.contains("win")) {
            return "cognee_java.dll";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return "libcognee_java.dylib";
        }
        return "libcognee_java.so";
    }

    private static String suffix() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (os.contains("win")) {
            return ".dll";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return ".dylib";
        }
        return ".so";
    }
}
