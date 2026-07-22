package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.LinkedHashMap;
import java.util.Map;

/** A single typed memory entry for {@link Cognee#rememberEntry}. */
public final class MemoryEntry {
    private final Map<String, Object> fields = new LinkedHashMap<>();

    private MemoryEntry(String type) {
        fields.put("type", type);
    }

    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    private MemoryEntry put(String key, Object value) {
        if (value != null) {
            fields.put(key, value);
        }
        return this;
    }

    // --- qa ---
    public static MemoryEntry qa(String question, String answer) {
        return new MemoryEntry("qa").put("question", question).put("answer", answer);
    }

    public MemoryEntry context(String c) { return put("context", c); }
    public MemoryEntry usedGraphElementIds(Map<String, ?> m) { return put("usedGraphElementIds", m); }

    // --- trace ---
    public static MemoryEntry trace(String originFunction) {
        return new MemoryEntry("trace").put("originFunction", originFunction);
    }

    public MemoryEntry status(String s) { return put("status", s); }
    public MemoryEntry memoryQuery(String q) { return put("memoryQuery", q); }
    public MemoryEntry memoryContext(String c) { return put("memoryContext", c); }
    public MemoryEntry methodParams(Object o) { return put("methodParams", o); }
    public MemoryEntry methodReturnValue(Object o) { return put("methodReturnValue", o); }
    public MemoryEntry errorMessage(String e) { return put("errorMessage", e); }
    public MemoryEntry generateFeedbackWithLlm(boolean b) { return put("generateFeedbackWithLlm", b); }

    // --- feedback ---
    public static MemoryEntry feedback(String qaId) {
        return new MemoryEntry("feedback").put("qaId", qaId);
    }

    // shared optional feedback fields (qa + feedback)
    public MemoryEntry feedbackText(String t) { return put("feedbackText", t); }
    public MemoryEntry feedbackScore(int s) { return put("feedbackScore", s); }
}
