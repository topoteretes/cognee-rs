package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeLifecycleTest {
    @Test
    void constructCloseRoundTrips(@TempDir Path dir) {
        Cognee cognee = new Cognee(TestConfig.underTempDir(dir));
        // Exercise the closed-guard via the real op-dispatch path (what every
        // op uses) rather than a back-door accessor.
        assertDoesNotThrow(() -> cognee.dispatch(h -> h));
        cognee.close();
        cognee.close(); // idempotent
        assertThrows(IllegalStateException.class, () -> cognee.dispatch(h -> h));
    }

    @Test
    void envOnlyConstruction() {
        try (Cognee cognee = new Cognee()) {
            assertDoesNotThrow(() -> cognee.dispatch(h -> h));
        }
    }

    @Test
    void invalidSettingsThrowsCogneeException() {
        CogneeException ex =
                assertThrows(CogneeException.class, () -> new Cognee("[\"not an object\"]"));
        org.junit.jupiter.api.Assertions.assertEquals("VALIDATION_ERROR", ex.code());
    }
}
