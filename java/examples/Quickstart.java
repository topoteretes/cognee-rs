/*
 * Quickstart.java — Runnable example: full add -> cognify -> search pipeline.
 *
 * This example is intentionally NOT part of the default `mvn verify` test run
 * (examples are opt-in, mirroring the TS/Python example pattern). Compile and
 * run it manually against the built classes and native library.
 *
 * Prerequisites
 * -------------
 * 1. Build the jar and the native library (see java/README.md "Install / build"):
 *      cargo build --manifest-path java/cognee-java-jni/Cargo.toml
 *      mvn -f java/pom.xml package -DskipTests
 * 2. Point the loader at the built cdylib (dev path):
 *      export COGNEE_JAVA_LIB_PATH="$(pwd)/java/cognee-java-jni/target/debug/libcognee_java.so"
 *    (.dylib on macOS, .dll on Windows)
 * 3. Set the LLM credentials (absent -> the example prints SKIP and exits 0):
 *      export OPENAI_URL=https://api.openai.com/v1   # or any OpenAI-compatible endpoint
 *      export OPENAI_TOKEN=sk-...                     # API key
 *      export OPENAI_MODEL=gpt-4o-mini               # optional (defaults to gpt-4o-mini)
 *
 * Running
 * -------
 *   CP="java/target/classes:$(mvn -q -f java/pom.xml dependency:build-classpath \
 *          -Dmdep.outputFile=/dev/stdout 2>/dev/null | tail -1)"
 *   javac -cp "$CP" -d "$TMPDIR/cognee-examples" java/examples/Quickstart.java
 *   java  -cp "$CP:$TMPDIR/cognee-examples" Quickstart
 */

import ai.cognee.AddResult;
import ai.cognee.Cognee;
import ai.cognee.CognifyResult;
import ai.cognee.DataInput;
import ai.cognee.SearchOptions;
import ai.cognee.SearchResponse;
import ai.cognee.SearchType;
import java.util.List;
import java.util.Map;

public final class Quickstart {
    private Quickstart() {}

    public static void main(String[] args) {
        // Credential gate: skip cleanly (exit 0) so this can run in CI without secrets.
        String llmEndpoint = System.getenv("OPENAI_URL");
        String llmApiKey = System.getenv("OPENAI_TOKEN");
        if (llmEndpoint == null || llmEndpoint.isEmpty()
                || llmApiKey == null || llmApiKey.isEmpty()) {
            System.out.println(
                    "SKIP: OPENAI_URL and OPENAI_TOKEN must be set to run this example.\n"
                            + "  export OPENAI_URL=https://api.openai.com/v1\n"
                            + "  export OPENAI_TOKEN=sk-...");
            return; // exit 0
        }

        String llmModel = System.getenv().getOrDefault("OPENAI_MODEL", "gpt-4o-mini");

        // Step 1: construct a Cognee instance. Settings keys are canonical
        // snake_case Settings field names; absent keys fall back to env + defaults.
        // Cognee is AutoCloseable — try-with-resources releases the native handle.
        try (Cognee cognee = new Cognee(Map.of("data_root_directory", "./data"))) {
            cognee.config().setLlmConfig(Map.of(
                    "llm_provider", "openai",
                    "llm_model", llmModel,
                    "llm_api_key", llmApiKey,
                    "llm_endpoint", llmEndpoint));

            // Step 2: warm up — builds engines and resolves the default user.
            System.out.println("Warming up cognee services...");
            cognee.warm().join();
            System.out.println("Owner ID: " + cognee.ownerId().join());

            // Step 3: add data to a named dataset.
            String dataset = "history";
            System.out.println("Adding text to dataset \"" + dataset + "\"...");
            AddResult added = cognee.add(
                    List.of(DataInput.text("Ada Lovelace wrote the first algorithm.")),
                    dataset).join();
            System.out.println("Added result: " + added);

            // Step 4: cognify — extract the knowledge graph (calls the LLM).
            System.out.println("Running cognify (this calls the LLM)...");
            CognifyResult cognified = cognee.cognify(dataset).join();
            System.out.println("Cognify result: " + cognified);

            // Step 5: search — synthesize an answer from graph context.
            String query = "Who wrote the first algorithm?";
            System.out.println("Searching: \"" + query + "\"");
            SearchResponse hits = cognee.search(query,
                    new SearchOptions().searchType(SearchType.GRAPH_COMPLETION)).join();
            System.out.println("Search result:");
            System.out.println(hits.raw().toPrettyString());
        }
    }
}
