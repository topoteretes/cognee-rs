package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeAsyncTest {
    private Cognee handle(Path dir) {
        return new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
    }

    @Test
    void warmAndOwnerIdComplete(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            assertDoesNotThrow(() -> cognee.warm().join());
            String owner = cognee.ownerId().join();
            assertNotNull(owner);
            assertDoesNotThrow(() -> UUID.fromString(owner)); // valid UUID
        }
    }

    @Test
    void repeatedWarmIsStableUnderXcheckJni(@TempDir Path dir) {
        // Runs many completions so a global-/local-ref leak would trip -Xcheck:jni.
        try (Cognee cognee = handle(dir)) {
            for (int i = 0; i < 50; i++) {
                cognee.warm().join();
            }
        }
    }

    @Test
    void exceptionalCompletionCarriesCogneeException(@TempDir Path dir) {
        // Point the LLM/embedding at nonsense so warm() fails, exercising the
        // exceptional-completion path (CogneeException via the cached class).
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString(),
                "vector_db_provider", "definitely-not-a-real-provider"))) {
            CompletionException ex =
                    assertThrows(CompletionException.class, () -> cognee.warm().join());
            org.junit.jupiter.api.Assertions.assertTrue(
                    ex.getCause() instanceof CogneeException,
                    "cause should be CogneeException, was: " + ex.getCause());
        }
    }
}
