package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeAddTest {
    @Test
    void addReturnsTypedResult(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            AddResult r = cognee.add(List.of(DataInput.text("hello cognee")), "ds").join();
            assertEquals("ds", r.datasetName());
            assertEquals(1, r.addedCount());
            // Re-adding the identical payload is a content-addressed duplicate.
            AddResult r2 = cognee.add(List.of(DataInput.text("hello cognee")), "ds").join();
            assertEquals(0, r2.addedCount());
            assertEquals(1, r2.deduplicatedCount());
            assertEquals(1, r2.deduplicated().size());
        }
    }
}
