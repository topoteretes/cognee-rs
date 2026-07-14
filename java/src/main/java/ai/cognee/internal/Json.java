package ai.cognee.internal;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

/** Shared JSON marshalling for the cognee Java SDK (internal). */
public final class Json {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private Json() {}

    /** Serialize any value to a JSON string; {@code null} → the string "null". */
    public static String toJson(Object value) {
        try {
            return value == null ? "null" : MAPPER.writeValueAsString(value);
        } catch (Exception e) {
            throw new IllegalArgumentException("failed to serialize to JSON", e);
        }
    }

    public static <T> T fromJson(String json, Class<T> type) {
        try {
            return MAPPER.readValue(json, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to deserialize JSON: " + json, e);
        }
    }

    public static <T> T fromJson(String json, TypeReference<T> type) {
        try {
            return MAPPER.readValue(json, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to deserialize JSON: " + json, e);
        }
    }

    public static <T> T fromNode(com.fasterxml.jackson.databind.JsonNode node, Class<T> type) {
        try {
            return MAPPER.treeToValue(node, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to convert JSON node to " + type, e);
        }
    }

    public static com.fasterxml.jackson.databind.JsonNode tree(String json) {
        try {
            return MAPPER.readTree(json);
        } catch (Exception e) {
            throw new IllegalStateException("failed to parse JSON tree: " + json, e);
        }
    }
}
