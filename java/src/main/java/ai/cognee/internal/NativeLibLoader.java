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
            Path lib = isWindows(osName()) ? cachedWindowsLib(in, libFile) : extractToTempFile(in);
            System.load(lib.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    /**
     * POSIX extraction: a fresh 0600 per-process temp file removed on JVM exit. On
     * Linux/macOS an mmapped .so/.dylib can still be unlinked while loaded, so
     * {@code deleteOnExit()} reliably reclaims it and nothing accumulates.
     */
    private static Path extractToTempFile(InputStream in) throws IOException {
        Path tmp = Files.createTempFile("cognee_java", suffix());
        tmp.toFile().deleteOnExit();
        Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
        return tmp;
    }

    /**
     * Windows extraction: a version-keyed cache dir reused across runs. Windows keeps
     * a loaded DLL locked and mapped until the JVM exits, so {@code deleteOnExit()}
     * cannot remove it and every run would orphan a fresh cognee_java*.dll in %TEMP%.
     * Keying the file on the bundled version makes extraction happen once; later runs
     * (and concurrent JVMs) reuse the same DLL.
     */
    private static Path cachedWindowsLib(InputStream in, String libFile) throws IOException {
        Path cacheDir = Path.of(System.getProperty("java.io.tmpdir"), "cognee_java-" + jarVersion());
        Files.createDirectories(cacheDir);
        Path lib = cacheDir.resolve(libFile);
        if (Files.isReadable(lib) && Files.size(lib) > 0) {
            return lib;
        }
        // Extract to a unique temp file in the same dir, then atomically publish it so
        // no partially written DLL is ever visible and concurrent extractors don't clash.
        Path tmp = Files.createTempFile(cacheDir, "cognee_java", ".dll");
        try {
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            Files.move(tmp, lib, StandardCopyOption.ATOMIC_MOVE);
        } catch (IOException e) {
            // Another JVM won the race and mapped/locked lib first; fall back to its copy.
            Files.deleteIfExists(tmp);
            if (Files.isReadable(lib) && Files.size(lib) > 0) {
                return lib;
            }
            throw e;
        }
        return lib;
    }

    /**
     * Classifier for the exactly four (os, arch) targets shipped by java-prebuild.yml:
     * linux-x86_64, linux-aarch_64, osx-aarch_64, windows-x86_64. Anything else fails
     * fast with a clear message instead of loading a mismatched native library.
     */
    private static String platformClassifier() {
        String os = osName();
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        boolean aarch64 = arch.equals("aarch64") || arch.equals("arm64");
        boolean x8664 = arch.equals("amd64") || arch.equals("x86_64") || arch.equals("x64");
        if (os.contains("linux")) {
            if (x8664) {
                return "linux-x86_64";
            }
            if (aarch64) {
                return "linux-aarch_64";
            }
        } else if (isMac(os)) {
            if (aarch64) {
                return "osx-aarch_64";
            }
        } else if (isWindows(os)) {
            if (x8664) {
                return "windows-x86_64";
            }
        }
        throw new UnsatisfiedLinkError(
                "unsupported platform: os=" + os + " arch=" + arch
                        + " (supported: linux-x86_64, linux-aarch_64, osx-aarch_64,"
                        + " windows-x86_64)");
    }

    private static String libFileName() {
        String os = osName();
        if (isWindows(os)) {
            return "cognee_java.dll";
        }
        if (isMac(os)) {
            return "libcognee_java.dylib";
        }
        return "libcognee_java.so";
    }

    private static String suffix() {
        String os = osName();
        if (isWindows(os)) {
            return ".dll";
        }
        if (isMac(os)) {
            return ".dylib";
        }
        return ".so";
    }

    private static String osName() {
        return System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
    }

    private static boolean isWindows(String os) {
        return os.contains("win");
    }

    private static boolean isMac(String os) {
        return os.contains("mac") || os.contains("darwin");
    }
}
