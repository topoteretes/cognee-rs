package ai.cognee;

import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Shared test configuration that isolates ALL cognee state under a JUnit
 * {@code @TempDir}, so tests are hermetic and {@code mvn verify} is idempotent
 * across repeated local runs.
 *
 * <p>Redirecting only {@code data_root_directory}/{@code system_root_directory}
 * is not enough: the relational DB URL defaults to the cwd-relative
 * {@code sqlite:./cognee.db?mode=rwc} and is not re-anchored under the system
 * root, so without an explicit {@code relational_db_url} the dedup/session
 * metadata would persist in {@code java/cognee.db} and leak between runs.
 */
final class TestConfig {
    private TestConfig() {}

    /** A mutable config map with every store rooted under {@code dir}; callers may add keys. */
    static Map<String, String> underTempDir(Path dir) {
        Map<String, String> cfg = new LinkedHashMap<>();
        cfg.put("data_root_directory", dir.resolve("data").toString());
        cfg.put("system_root_directory", dir.resolve("sys").toString());
        cfg.put(
                "relational_db_url",
                "sqlite://" + dir.resolve("cognee.db").toAbsolutePath() + "?mode=rwc");
        return cfg;
    }
}
