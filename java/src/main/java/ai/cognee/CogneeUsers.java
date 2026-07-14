package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import java.util.concurrent.CompletableFuture;

/** User/admin operations (default user + pipeline-run resets). */
public final class CogneeUsers {
    private final Cognee cognee;

    CogneeUsers(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<CogneeUser> getOrCreateDefault() {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.getOrCreateDefaultUser(h, f));
        return f.thenApply(json -> Json.fromJson(json, CogneeUser.class));
    }

    public CompletableFuture<Void> resetPipelineRunStatus(String datasetId, String pipelineName) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.resetPipelineRunStatus(h, datasetId, pipelineName, f));
        return f.thenApply(s -> null);
    }

    public CompletableFuture<Void> resetDatasetPipelineRunStatus(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.resetDatasetPipelineRunStatus(h, datasetId, f));
        return f.thenApply(s -> null);
    }
}
