package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import java.util.Map;

/**
 * Synchronous configuration surface (design decision A3.1): generic {@link #set},
 * {@link #setStr}, four bulk setters, and {@link #get}. Keys are canonical
 * snake_case {@code Settings} field names. A type error throws
 * {@link CogneeException} with {@code code() == "CONFIG_TYPE_MISMATCH"}; an
 * unknown key throws with {@code "UNKNOWN_CONFIG_KEY"}.
 */
public final class CogneeConfig {
    private final Cognee cognee;

    CogneeConfig(Cognee cognee) {
        this.cognee = cognee;
    }

    /** Set any config key to any JSON-serializable value. */
    public void set(String key, Object value) {
        Native.configSet(cognee.handle(), key, Json.toJson(value));
    }

    /** Convenience for string-valued keys (identical to {@link #set}). */
    public void setStr(String key, String value) {
        set(key, value);
    }

    public void setLlmConfig(Map<String, ?> values) {
        Native.configSetLlmConfig(cognee.handle(), Json.toJson(values));
    }

    public void setEmbeddingConfig(Map<String, ?> values) {
        Native.configSetEmbeddingConfig(cognee.handle(), Json.toJson(values));
    }

    public void setVectorDbConfig(Map<String, ?> values) {
        Native.configSetVectorDbConfig(cognee.handle(), Json.toJson(values));
    }

    public void setGraphDbConfig(Map<String, ?> values) {
        Native.configSetGraphDbConfig(cognee.handle(), Json.toJson(values));
    }

    /** Read-back of the current settings (secret fields blanked, snake_case keys). */
    public Map<String, Object> get() {
        String json = Native.getConfig(cognee.handle());
        return Json.fromJson(json, new TypeReference<Map<String, Object>>() {});
    }
}
