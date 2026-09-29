package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/**
 * The verdict from {@link Cognee#classifyIntent(String)}: is this message a
 * question to answer, or a note to keep?
 *
 * <p>{@link #intent()} is always one of {@code "question"} or {@code "note"} —
 * never empty, never anything else. When the model produced nothing usable,
 * or the call failed outright, it reads {@code "question"} and
 * {@link #fallback()} is true. That direction is deliberate: answering a
 * message that was only a note is visible and cheap, while filing a question
 * away as a note loses it. Callers that care how often the model is actually
 * deciding should read {@link #fallback()} rather than assume.
 */
public final class IntentResult {
    /** The verdict for a message the person wants answered. */
    public static final String QUESTION = "question";
    /** The verdict for a message the person wants kept. */
    public static final String NOTE = "note";

    private final JsonNode root;

    IntentResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }

    /** {@link #QUESTION} or {@link #NOTE}; never null. */
    public String intent() {
        String value = root.path("intent").asText(QUESTION);
        return NOTE.equals(value) ? NOTE : QUESTION;
    }

    /** Whether {@link #intent()} is the safe default rather than a model verdict. */
    public boolean fallback() {
        return root.path("fallback").asBoolean(false);
    }

    /** Wall-clock milliseconds the classification took. */
    public long elapsedMs() {
        return root.path("elapsedMs").asLong(0L);
    }

    /** The adapter's error message when the call failed, else null. */
    public String error() {
        return root.path("error").asText(null);
    }

    public boolean isNote() {
        return NOTE.equals(intent());
    }
}
