package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/** Dataset-management operations. */
public final class CogneeDatasets {
    private final Cognee cognee;

    CogneeDatasets(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<CogneeDataset>> list() {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.listDatasets(h, f));
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<List<CogneeDataset>>() {}));
    }

    public CompletableFuture<List<CogneeData>> listData(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.listData(h, datasetId, f));
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<List<CogneeData>>() {}));
    }

    public CompletableFuture<Boolean> has(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.hasData(h, datasetId, f));
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    public CompletableFuture<Map<String, Object>> status(List<String> datasetIds) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.datasetStatus(h, Json.toJson(datasetIds), f));
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<Map<String, Object>>() {}));
    }

    public CompletableFuture<DeleteResult> empty(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.emptyDataset(h, datasetId, f));
        return f.thenApply(json -> new DeleteResult(Json.tree(json)));
    }

    public CompletableFuture<DeleteResult> deleteData(String datasetId, String dataId) {
        return deleteData(datasetId, dataId, null);
    }

    public CompletableFuture<DeleteResult> deleteData(
            String datasetId, String dataId, DeleteDataOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.deleteData(h, datasetId, dataId, Options.jsonOf(opts), f));
        return f.thenApply(json -> new DeleteResult(Json.tree(json)));
    }

    public CompletableFuture<List<DeleteResult>> deleteAll() {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.deleteAllDatasets(h, f));
        return f.thenApply(json -> {
            List<DeleteResult> out = new ArrayList<>();
            for (JsonNode n : Json.tree(json)) {
                out.add(new DeleteResult(n));
            }
            return out;
        });
    }
}
