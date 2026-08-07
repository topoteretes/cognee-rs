package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeConfigTest {
    private Cognee handle(Path dir) {
        return new Cognee(TestConfig.underTempDir(dir));
    }

    @Test
    void setAndGetRoundTrip(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            cognee.config().set("llm_model", "gpt-4o-mini");
            cognee.config().setLlmConfig(Map.of("llm_provider", "openai"));
            Map<String, Object> snapshot = cognee.config().get();
            assertEquals("gpt-4o-mini", snapshot.get("llm_model"));
        }
    }

    @Test
    void typeMismatchSurfacesCode(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeException ex = assertThrows(
                    CogneeException.class,
                    () -> cognee.config().set("chunk_size", "not-a-number"));
            assertEquals("CONFIG_TYPE_MISMATCH", ex.code());
        }
    }

    @Test
    void unknownKeySurfacesCode(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeException ex = assertThrows(
                    CogneeException.class,
                    () -> cognee.config().set("no_such_key", "x"));
            assertEquals("UNKNOWN_CONFIG_KEY", ex.code());
        }
    }
}
