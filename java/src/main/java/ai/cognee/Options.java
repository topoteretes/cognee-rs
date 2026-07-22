package ai.cognee;

import ai.cognee.internal.Json;
import java.util.LinkedHashMap;
import java.util.Map;

/** Base for typed option builders. Serializes to the camelCase {@code opts} JSON. */
public abstract class Options {
    protected final Map<String, Object> values = new LinkedHashMap<>();

    protected void put(String key, Object value) {
        if (value != null) {
            values.put(key, value);
        }
    }

    /** The JSON this builder sends across the boundary. */
    public String toJson() {
        return Json.toJson(values);
    }

    /** JSON for an options builder, or {@code "null"} when none was given. */
    static String jsonOf(Options opts) {
        return opts == null ? "null" : opts.toJson();
    }
}
