package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class SessionsAdminTest {
    private Cognee handle(Path dir) {
        return new Cognee(TestConfig.underTempDir(dir));
    }

    @Test
    void defaultUserResolves(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeUser u = cognee.users().getOrCreateDefault().join();
            assertNotNull(u.id());
        }
    }
}
