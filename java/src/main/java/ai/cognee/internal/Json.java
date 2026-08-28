package ai.cognee.internal;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

/** Shared JSON marshalling for the cognee Java SDK (internal). */
public final class Json {
    private static final ObjectMapper MAPPER = newMapper();

    /**
     * The shared mapper.
     *
     * <p>On a JVM this is a plain {@code ObjectMapper}, exactly as before — no
     * module discovery, so host classpath contents cannot alter this binding's
     * marshalling.
     *
     * <p>On Android it additionally registers whatever Jackson modules are on the
     * classpath. D8 desugars this SDK's result {@code record}s away when minSdk is
     * below 34, stripping the record metadata Jackson's built-in record support
     * relies on; deserialization then fails with "no Creators, like default
     * constructor, exist". With {@code jackson-module-parameter-names} packaged
     * (and {@code -parameters} at compile time) the canonical constructor is used
     * as a property-based creator instead.
     */
    private static ObjectMapper newMapper() {
        ObjectMapper mapper = new ObjectMapper();
        String vm = System.getProperty("java.vm.name", "");
        if (vm.contains("Dalvik") || vm.contains("ART")) {
            mapper.findAndRegisterModules();
        }
        return mapper;
    }

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
