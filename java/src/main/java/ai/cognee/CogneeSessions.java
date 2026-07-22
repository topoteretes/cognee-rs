package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import java.util.List;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/** Session-management operations (QA history, feedback, graph context). */
public final class CogneeSessions {
    private final Cognee cognee;

    CogneeSessions(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<Map<String, Object>>> get(String sessionId) {
        return get(sessionId, null);
    }

    /** {@code lastN} limits the number of returned QA entries (null = all). */
    public CompletableFuture<List<Map<String, Object>>> get(String sessionId, Integer lastN) {
        String opts = lastN == null ? "null" : Json.toJson(Map.of("lastN", lastN));
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.getSession(h, sessionId, opts, f));
        return f.thenApply(json ->
                Json.fromJson(json, new TypeReference<List<Map<String, Object>>>() {}));
    }

    public CompletableFuture<Boolean> addFeedback(
            String sessionId, String qaId, String feedbackText, Integer feedbackScore) {
        Map<String, Object> opts = new LinkedHashMap<>();
        if (feedbackText != null) opts.put("feedbackText", feedbackText);
        if (feedbackScore != null) opts.put("feedbackScore", feedbackScore);
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.addFeedback(h, sessionId, qaId, Json.toJson(opts), f));
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    public CompletableFuture<Boolean> deleteFeedback(String sessionId, String qaId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.deleteFeedback(h, sessionId, qaId, f));
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    /** Returns the stored graph context, or null if none. */
    public CompletableFuture<String> getGraphContext(String sessionId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.getGraphContext(h, sessionId, f));
        // The op completes with a JSON string ("..." or null).
        return f.thenApply(json -> Json.fromJson(json, String.class));
    }

    public CompletableFuture<Void> setGraphContext(String sessionId, String context) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.setGraphContext(h, sessionId, context, f));
        return f.thenApply(s -> null);
    }
}
