package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ai.cognee.internal.Json;
import org.junit.jupiter.api.Test;

class SearchTypeTest {
    @Test
    void wireValuesAreConstantNames() {
        assertEquals("GRAPH_COMPLETION", SearchType.GRAPH_COMPLETION.wire());
        assertEquals(SearchType.CHUNKS_LEXICAL, SearchType.fromWire("CHUNKS_LEXICAL"));
        assertEquals(15, SearchType.values().length);
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
}
