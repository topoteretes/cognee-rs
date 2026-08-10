package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
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

    /**
     * Regression test for issue #132: {@code close()} must return with the SQLite
     * {@code -wal}/{@code -shm} sidecars already gone. They used to survive until
     * the JVM exited — a connection leak, and a race against {@code @TempDir}
     * cleanup, which walks this very directory as soon as the test returns.
     */
    @Test
    void closeReleasesSqliteSidecars(@TempDir Path dir) {
        Path db = dir.resolve("cognee.db");
        Path wal = dir.resolve("cognee.db-wal");
        Path shm = dir.resolve("cognee.db-shm");

        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            // Constructing a handle opens nothing; an op is what opens the DB.
            assertFalse(Files.exists(db), "construction must not open the database");
            cognee.warm().join();
            assertTrue(Files.exists(wal) && Files.exists(shm),
                    "a warmed handle runs the relational database in WAL mode");
        }

        // No waiting, no polling: close() is synchronous about this.
        assertFalse(Files.exists(wal), "close() must release cognee.db-wal");
        assertFalse(Files.exists(shm), "close() must release cognee.db-shm");
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
