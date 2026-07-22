package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/** Notebook-management operations. */
public final class CogneeNotebooks {
    private final Cognee cognee;

    CogneeNotebooks(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<CogneeNotebook>> list() {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.listNotebooks(h, f));
        return f.thenApply(json -> {
            List<CogneeNotebook> out = new ArrayList<>();
            for (JsonNode n : Json.tree(json)) {
                out.add(new CogneeNotebook(n));
            }
            return out;
        });
    }

    public CompletableFuture<CogneeNotebook> create(String name) {
        return create(name, null, true);
    }

    public CompletableFuture<CogneeNotebook> create(String name, List<?> cells, boolean deletable) {
        String cellsJson = cells == null ? "null" : Json.toJson(cells);
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.createNotebook(h, name, cellsJson, deletable, f));
        return f.thenApply(json -> new CogneeNotebook(Json.tree(json)));
    }

    /** Returns the updated notebook, or null if not found. */
    public CompletableFuture<CogneeNotebook> update(String id, String name, List<?> cells) {
        Map<String, Object> patch = new LinkedHashMap<>();
        if (name != null) patch.put("name", name);
        if (cells != null) patch.put("cells", cells);
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.updateNotebook(h, id, Json.toJson(patch), f));
        return f.thenApply(json -> {
            JsonNode n = Json.tree(json);
            return n.isNull() ? null : new CogneeNotebook(n);
        });
    }

    public CompletableFuture<Boolean> delete(String id) {
        CompletableFuture<String> f = new CompletableFuture<>();
        cognee.dispatchVoid(h -> Native.deleteNotebook(h, id, f));
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }
}
