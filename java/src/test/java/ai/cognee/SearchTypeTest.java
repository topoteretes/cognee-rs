package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import org.junit.jupiter.api.Test;

class SearchTypeTest {
    @Test
    void wireValuesAreConstantNames() {
        assertEquals("GRAPH_COMPLETION", SearchType.GRAPH_COMPLETION.wire());
        assertEquals(SearchType.CHUNKS_LEXICAL, SearchType.fromWire("CHUNKS_LEXICAL"));
    }

    @Test
    void fromWireOrNullIsForwardCompatible() {
        assertEquals(SearchType.GRAPH_COMPLETION, SearchType.fromWireOrNull("GRAPH_COMPLETION"));
        // A future core-added type must not crash deserialization.
        assertNull(SearchType.fromWireOrNull("SOME_FUTURE_TYPE"));
        assertNull(SearchType.fromWireOrNull(""));
        assertNull(SearchType.fromWireOrNull(null));
    }

    @Test
    void searchResponseToleratesUnknownAndMissingType() {
        SearchResponse unknown = new SearchResponse(
                Json.tree("{\"search_type\":\"SOME_FUTURE_TYPE\",\"result\":{}}"));
        assertNull(unknown.searchType());
        SearchResponse missing = new SearchResponse(Json.tree("{\"result\":{}}"));
        assertNull(missing.searchType());
    }

    @Test
    void searchResponseParsesCannedJson() {
        String canned = "{\"search_type\":\"GRAPH_COMPLETION\",\"result\":{\"kind\":\"Text\","
                + "\"data\":\"hello\"},\"only_context\":false,\"use_combined_context\":false,"
                + "\"verbose\":true}";
        SearchResponse r = new SearchResponse(Json.tree(canned));
        assertEquals(SearchType.GRAPH_COMPLETION, r.searchType());
        assertTrue(r.verbose());
        assertEquals("Text", r.result().path("kind").asText());
    }

    @Test
    void recallResultParsesCannedJson() {
        String canned = "{\"items\":[{\"text\":\"remembered\"}],"
                + "\"searchTypeUsed\":\"GRAPH_COMPLETION\",\"autoRouted\":true,"
                + "\"searchResponse\":{\"search_type\":\"CHUNKS\",\"result\":{\"kind\":\"Text\","
                + "\"data\":\"hi\"},\"verbose\":false}}";
        RecallResult r = new RecallResult(Json.tree(canned));
        assertEquals(SearchType.GRAPH_COMPLETION, r.searchTypeUsed());
        assertTrue(r.autoRouted());
        assertEquals(1, r.items().size());
        assertEquals("remembered", r.items().get(0).path("text").asText());
        SearchResponse nested = r.searchResponse();
        assertEquals(SearchType.CHUNKS, nested.searchType());
    }

    @Test
    void recallResultToleratesMissingAndUnknownFields() {
        RecallResult empty = new RecallResult(Json.tree("{}"));
        assertNull(empty.searchTypeUsed());
        assertNull(empty.searchResponse());
        assertTrue(empty.items().isMissingNode() || empty.items().size() == 0);
        RecallResult future = new RecallResult(
                Json.tree("{\"searchTypeUsed\":\"SOME_FUTURE_TYPE\"}"));
        assertNull(future.searchTypeUsed());
    }

    @Test
    void recallOptionsSerializeToCamelCase() {
        RecallOptions opts = new RecallOptions()
                .searchType(SearchType.GRAPH_COMPLETION)
                .datasets(List.of("ds"))
                .topK(7)
                .autoRoute(true)
                .sessionId("sess-1")
                .scope(List.of("graph", "session"));
        JsonNode n = Json.tree(opts.toJson());
        assertEquals("GRAPH_COMPLETION", n.path("searchType").asText());
        assertEquals("ds", n.path("datasets").get(0).asText());
        assertEquals(7, n.path("topK").asInt());
        assertTrue(n.path("autoRoute").asBoolean());
        assertEquals("sess-1", n.path("sessionId").asText());
        assertEquals("session", n.path("scope").get(1).asText());
    }
}
