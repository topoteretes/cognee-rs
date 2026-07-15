package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class DatasetsTest {
    @Test
    void addThenListIsDeterministic(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            cognee.add(List.of(DataInput.text("x")), "ds").join();
            List<CogneeDataset> ds = cognee.datasets().list().join();
            CogneeDataset d = ds.stream()
                    .filter(x -> "ds".equals(x.name())).findFirst().orElseThrow();
            // Dataset id is uuid5(NAMESPACE_OID, "ds" + defaultOwnerId + "None") where the
            // default owner id is uuid5(NAMESPACE_OID, "default_user@example.com"). This is
            // content-addressed, so it is byte-for-byte stable across runs and SDKs.
            assertEquals("7bc453ac-8dae-5d5d-8fde-9e9f69a874ce", d.id());
            assertEquals(Boolean.TRUE, cognee.datasets().has(d.id()).join());
        }
    }
}
