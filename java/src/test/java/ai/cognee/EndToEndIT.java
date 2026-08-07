package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class EndToEndIT {
    @Test
    void warmAddCognifySearch(@TempDir Path dir) {
        String url = System.getenv("OPENAI_URL");
        String token = System.getenv("OPENAI_TOKEN");
        assumeTrue(url != null && !url.isEmpty() && token != null && !token.isEmpty(),
                "OPENAI_URL/OPENAI_TOKEN not set — skipping LLM E2E");

        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            cognee.config().setLlmConfig(Map.of(
                    "llm_provider", "openai", "llm_api_key", token, "llm_endpoint", url));
            cognee.warm().join();
            cognee.add(List.of(DataInput.text(
                    "Alan Turing was a mathematician who founded computer science.")),
                    "ds").join();
            CognifyResult c = cognee.cognify("ds").join();
            assertNotNull(c);
            SearchResponse r = cognee.search("Who founded computer science?",
                    new SearchOptions().searchType(SearchType.GRAPH_COMPLETION)).join();
            assertNotNull(r.raw());
        }
    }
}
