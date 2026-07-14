package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeLifecycleTest {
    @Test
    void constructCloseRoundTrips(@TempDir Path dir) {
        Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
        assertDoesNotThrow(cognee::handle);
        cognee.close();
        cognee.close(); // idempotent
        assertThrows(IllegalStateException.class, cognee::handle);
    }

    @Test
    void envOnlyConstruction() {
        try (Cognee cognee = new Cognee()) {
            assertDoesNotThrow(cognee::handle);
        }
    }

    @Test
    void invalidSettingsThrowsCogneeException() {
        CogneeException ex =
                assertThrows(CogneeException.class, () -> new Cognee("[\"not an object\"]"));
        org.junit.jupiter.api.Assertions.assertEquals("VALIDATION_ERROR", ex.code());
    }
}
