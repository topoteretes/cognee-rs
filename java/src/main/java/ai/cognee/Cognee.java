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
    private CogneeDatasets datasets;
    private CogneeSessions sessions;
    private CogneeUsers users;
    private CogneeNotebooks notebooks;

    /** The synchronous configuration surface. */
    public synchronized CogneeConfig config() {
        if (config == null) {
            config = new CogneeConfig(this);
        }
        return config;
    }

    /** The dataset-management surface. */
    public synchronized CogneeDatasets datasets() {
        if (datasets == null) {
            datasets = new CogneeDatasets(this);
        }
        return datasets;
    }

    /** The session-management surface. */
    public synchronized CogneeSessions sessions() {
        if (sessions == null) sessions = new CogneeSessions(this);
        return sessions;
    }

    /** The user/admin surface (default user + pipeline-run resets). */
    public synchronized CogneeUsers users() {
        if (users == null) users = new CogneeUsers(this);
        return users;
    }

    /** The notebook-management surface. */
    public synchronized CogneeNotebooks notebooks() {
        if (notebooks == null) notebooks = new CogneeNotebooks(this);
        return notebooks;
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
    /** Ingest inputs into {@code datasetName} (creating it if needed). */
    public CompletableFuture<AddResult> add(java.util.List<DataInput> inputs, String datasetName) {
        return add(inputs, datasetName, null);
    }

    /** Ingest inputs into {@code datasetName} with per-call {@link AddOptions}. */
    public CompletableFuture<AddResult> add(
            java.util.List<DataInput> inputs, String datasetName, AddOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.add(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, AddResult.class));
    }

    /** Ingest a single {@link DataInput}. */
    public CompletableFuture<AddResult> add(DataInput input, String datasetName, AddOptions opts) {
        return add(java.util.List.of(input), datasetName, opts);
    }

    /** Ingest a single text snippet (shorthand for {@link DataInput#text}). */
    public CompletableFuture<AddResult> add(String text, String datasetName) {
        return add(DataInput.text(text), datasetName, null);
    }

    // --- cognify ---
    /** Extract the knowledge graph for a dataset (entities, relationships, summaries). */
    public CompletableFuture<CognifyResult> cognify(String datasetName) {
        return cognify(datasetName, null);
    }

    /** Extract the knowledge graph with per-call {@link CognifyOptions}. */
    public CompletableFuture<CognifyResult> cognify(String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.cognify(handle(), datasetName, Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, CognifyResult.class));
    }

    // --- addAndCognify ---
    /** Ingest and extract in a single call. */
    public CompletableFuture<AddAndCognifyResult> addAndCognify(
            java.util.List<DataInput> inputs, String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.addAndCognify(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(
                json -> ai.cognee.internal.Json.fromJson(json, AddAndCognifyResult.class));
    }

    // --- search ---
    /** Query the knowledge graph (defaults to {@code GRAPH_COMPLETION}). */
    public CompletableFuture<SearchResponse> search(String query) {
        return search(query, null);
    }

    /** Query the knowledge graph with per-call {@link SearchOptions}. */
    public CompletableFuture<SearchResponse> search(String query, SearchOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.search(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new SearchResponse(ai.cognee.internal.Json.tree(json)));
    }

    // --- recall ---
    /** Source-aware retrieval: checks session history before falling back to graph search. */
    public CompletableFuture<RecallResult> recall(String query) {
        return recall(query, null);
    }

    /** Source-aware retrieval with per-call {@link RecallOptions}. */
    public CompletableFuture<RecallResult> recall(String query, RecallOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.recall(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new RecallResult(ai.cognee.internal.Json.tree(json)));
    }

    // --- remember ---
    /** Composite ingest + extract (with an optional self-improvement pass). */
    public CompletableFuture<RememberResult> remember(
            java.util.List<DataInput> inputs, String datasetName, RememberOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.remember(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> new RememberResult(ai.cognee.internal.Json.tree(json)));
    }

    // --- rememberEntry ---
    /** Store a typed {@link MemoryEntry} (qa/trace/feedback) in a session. */
    public CompletableFuture<RememberResult> rememberEntry(
            MemoryEntry entry, String datasetName, String sessionId, String tenant) {
        String optsJson = tenant == null ? "null"
                : ai.cognee.internal.Json.toJson(java.util.Map.of("tenant", tenant));
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.rememberEntry(handle(), ai.cognee.internal.Json.toJson(entry), datasetName,
                sessionId, optsJson, f);
        return f.thenApply(json -> new RememberResult(ai.cognee.internal.Json.tree(json)));
    }

    // --- memify ---
    /** Index triplet embeddings from the existing graph (enables triplet search). */
    public CompletableFuture<MemifyResult> memify(MemifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.memify(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, MemifyResult.class));
    }

    /** Index triplet embeddings with default options. */
    public CompletableFuture<MemifyResult> memify() {
        return memify(null);
    }

    // --- improve ---
    /** Run the session-graph bridge pipeline ({@link ImproveOptions}'s {@code datasetName} is required). */
    public CompletableFuture<ImproveResult> improve(ImproveOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.improve(handle(), opts.toJson(), f); // opts required (datasetName)
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, ImproveResult.class));
    }

    // --- forget ---
    /** Delete an item, a dataset, or everything (see {@link ForgetTarget}). */
    public CompletableFuture<ForgetResult> forget(ForgetTarget target) {
        return forget(target, null);
    }

    /** Forget a target scoped to a specific {@code tenant}. */
    public CompletableFuture<ForgetResult> forget(ForgetTarget target, String tenant) {
        String optsJson = tenant == null ? "null"
                : ai.cognee.internal.Json.toJson(java.util.Map.of("tenant", tenant));
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.forget(handle(), ai.cognee.internal.Json.toJson(target), optsJson, f);
        return f.thenApply(json -> new ForgetResult(ai.cognee.internal.Json.tree(json)));
    }

    // --- update ---
    /** Replace a data item (delete then re-add and re-cognify). */
    public CompletableFuture<UpdateResult> update(
            String dataId, java.util.List<DataInput> newData, String datasetName, UpdateOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.update(handle(), dataId, ai.cognee.internal.Json.toJson(newData), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> new UpdateResult(ai.cognee.internal.Json.tree(json)));
    }

    // --- pruneData ---
    /** Remove all ingested files from storage (metadata DB untouched). */
    public CompletableFuture<Void> pruneData() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.pruneData(handle(), f);
        return f.thenApply(s -> null);
    }

    // --- pruneSystem ---
    /** Wipe the selected backends (graph/vector/metadata/cache) per {@link PruneSystemOptions}. */
    public CompletableFuture<PruneResult> pruneSystem(PruneSystemOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.pruneSystem(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, PruneResult.class));
    }

    /** Wipe all system backends with default options. */
    public CompletableFuture<PruneResult> pruneSystem() {
        return pruneSystem(null);
    }

    // --- visualization ---
    /** Render the knowledge graph to an HTML string. */
    public CompletableFuture<String> visualize() {
        return visualize(null);
    }

    /** Render the knowledge graph to an HTML string with per-call {@link VisualizeOptions}. */
    public CompletableFuture<String> visualize(VisualizeOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.visualize(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }

    /** Render the graph to a file (returns the absolute path written). */
    public CompletableFuture<String> visualizeToFile(VisualizeOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.visualizeToFile(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }

    // --- module-level statics ---
    /** Initialize file logging from env vars (idempotent). */
    public static void setupLogging() {
        Native.setupLogging();
    }

    /** Install OpenTelemetry OTLP export from env vars (idempotent). */
    public static void initOtlp() {
        Native.initOtlp();
    }

    /** Arm product-analytics emission (per the opt-out policy); returns whether
     *  analytics are effective for this process. */
    public static boolean initTelemetry() {
        return Native.initTelemetry();
    }

    /** The native/SDK version string. */
    public static String version() {
        return Native.version();
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
