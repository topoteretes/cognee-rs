package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class DatasetsTest {
    @Test
    void addThenListIsDeterministic(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()))) {
            cognee.add(List.of(DataInput.text("x")), "ds").join();
            List<CogneeDataset> ds = cognee.datasets().list().join();
            assertTrue(ds.stream().anyMatch(d -> "ds".equals(d.name())));
            assertEquals(Boolean.TRUE, cognee.datasets()
                    .has(ds.stream().filter(d -> "ds".equals(d.name())).findFirst().get().id())
                    .join());
        }
    }
}
