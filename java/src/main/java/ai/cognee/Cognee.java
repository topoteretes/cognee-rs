package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import java.lang.ref.Cleaner;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/**
 * The cognee Java SDK entry point. Construct with optional settings (canonical
 * snake_case {@code Settings} field names), then drive the pipeline. Holds a
 * native handle; call {@link #close()} to release it (a {@link Cleaner} is a
 * leak backstop, but {@code close()} is the primary path).
 */
public final class Cognee implements AutoCloseable {
    private static final Cleaner CLEANER = Cleaner.create();

    /** Mutable holder so the Cleaner can null the handle after freeing it. */
    private static final class Handle implements Runnable {
        private long ptr;

        Handle(long ptr) {
            this.ptr = ptr;
        }

        @Override
        public void run() {
            if (ptr != 0) {
                Native.destroy(ptr);
                ptr = 0;
            }
        }
    }

    private final Handle handleHolder;
    private final Cleaner.Cleanable cleanable;
    private volatile boolean closed = false;
    private CogneeConfig config;

    /** The synchronous configuration surface. */
    public synchronized CogneeConfig config() {
        if (config == null) {
            config = new CogneeConfig(this);
        }
        return config;
    }

    /** Construct from environment/default settings. */
    public Cognee() {
        this((String) null);
    }

    /** Construct from a settings map (canonical snake_case keys). */
    public Cognee(Map<String, ?> settings) {
        this(settings == null ? null : Json.toJson(settings));
    }

    /** Construct from a settings JSON string (or {@code null} for env-only). */
    public Cognee(String settingsJson) {
        long ptr = Native.newHandle(settingsJson); // throws CogneeException on bad settings
        this.handleHolder = new Handle(ptr);
        this.cleanable = CLEANER.register(this, this.handleHolder);
    }

    /** The native handle for internal op calls. Throws if closed. */
    public long handle() {
        if (closed) {
            throw new IllegalStateException("Cognee handle is closed");
        }
        return handleHolder.ptr;
    }

    /** Force engine construction now (surfaces config/connection errors early). */
    public CompletableFuture<Void> warm() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.warm(handle(), f);
        return f.thenApply(s -> null);
    }

    /** The email-derived owner id (warms lazily if needed). */
    public CompletableFuture<String> ownerId() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.ownerId(handle(), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }

    // --- add ---
    public CompletableFuture<AddResult> add(java.util.List<DataInput> inputs, String datasetName) {
        return add(inputs, datasetName, null);
    }

    public CompletableFuture<AddResult> add(
            java.util.List<DataInput> inputs, String datasetName, AddOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.add(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, AddResult.class));
    }

    public CompletableFuture<AddResult> add(DataInput input, String datasetName, AddOptions opts) {
        return add(java.util.List.of(input), datasetName, opts);
    }

    public CompletableFuture<AddResult> add(String text, String datasetName) {
        return add(DataInput.text(text), datasetName, null);
    }

    // --- cognify ---
    public CompletableFuture<CognifyResult> cognify(String datasetName) {
        return cognify(datasetName, null);
    }

    public CompletableFuture<CognifyResult> cognify(String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.cognify(handle(), datasetName, Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, CognifyResult.class));
    }

    // --- addAndCognify ---
    public CompletableFuture<AddAndCognifyResult> addAndCognify(
            java.util.List<DataInput> inputs, String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.addAndCognify(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(
                json -> ai.cognee.internal.Json.fromJson(json, AddAndCognifyResult.class));
    }

    // --- search ---
    public CompletableFuture<SearchResponse> search(String query) {
        return search(query, null);
    }

    public CompletableFuture<SearchResponse> search(String query, SearchOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.search(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new SearchResponse(ai.cognee.internal.Json.tree(json)));
    }

    // --- recall ---
    public CompletableFuture<RecallResult> recall(String query) {
        return recall(query, null);
    }

    public CompletableFuture<RecallResult> recall(String query, RecallOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.recall(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new RecallResult(ai.cognee.internal.Json.tree(json)));
    }

    @Override
    public void close() {
        if (closed) {
            return;
        }
        closed = true;
        cleanable.clean(); // runs Handle.run() exactly once → Native.destroy
    }
}
