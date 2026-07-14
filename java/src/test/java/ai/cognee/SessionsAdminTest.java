package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class SessionsAdminTest {
    private Cognee handle(Path dir) {
        return new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
    }

    @Test
    void defaultUserResolves(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeUser u = cognee.users().getOrCreateDefault().join();
            assertNotNull(u.id());
        }
    }

    @Test
    void notebookCrud(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeNotebook nb = cognee.notebooks().create("nb1").join();
            assertEquals("nb1", nb.name());
            assertTrue(cognee.notebooks().delete(nb.id()).join());
        }
    }
}
