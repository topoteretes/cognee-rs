package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.Base64;
import java.util.Map;

/** A discriminated input for {@code add}/{@code addAndCognify}/{@code remember}/{@code update}. */
public final class DataInput {
    private final Map<String, Object> fields;

    private DataInput(Map<String, Object> fields) {
        this.fields = fields;
    }

    /** The `{type,…}` object Jackson serializes for this input. */
    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    public static DataInput text(String text) {
        return new DataInput(Map.of("type", "text", "text", text));
    }

    public static DataInput file(String path) {
        return new DataInput(Map.of("type", "file", "path", path));
    }

    public static DataInput url(String url) {
        return new DataInput(Map.of("type", "url", "url", url));
    }

    /** Binary input; {@code name} drives MIME detection. Bytes are sent base64. */
    public static DataInput binary(byte[] bytes, String name) {
        String b64 = Base64.getEncoder().encodeToString(bytes);
        return new DataInput(Map.of("type", "binary", "bytes", b64, "name", name));
    }
}
