package ai.cognee;

import java.util.List;

/** Per-call options for {@link Cognee#recall}. */
public final class RecallOptions extends Options {
    public RecallOptions searchType(SearchType t) { if (t != null) put("searchType", t.wire()); return this; }
    public RecallOptions datasets(List<String> d) { put("datasets", d); return this; }
    /**
     * Dataset UUIDs to restrict graph search to. Takes precedence over
     * {@link #datasets}; an empty list is no filter. Every entry must be a
     * valid UUID — a malformed one is rejected rather than dropped, so a typo
     * cannot silently widen the query.
     */
    public RecallOptions datasetIds(List<String> ids) { put("datasetIds", ids); return this; }
    /**
     * Tenant UUID, matching the {@code tenant} passed to {@code add}/{@code cognify}.
     * Scopes {@link #datasets} name resolution: one handle can hold same-named
     * datasets in several tenants. Omit for the single-tenant default.
     */
    public RecallOptions tenant(String tenant) { put("tenant", tenant); return this; }
    public RecallOptions topK(int n) { put("topK", n); return this; }
    public RecallOptions autoRoute(boolean b) { put("autoRoute", b); return this; }
    public RecallOptions sessionId(String s) { put("sessionId", s); return this; }
    /**
     * A single scope: "auto", "graph", "session", "session_first",
     * "session_context", "trace" or "all" ("graph_context" is a deprecated
     * alias of "graph"). "tools" and "code" are accepted names that fail
     * validation as not supported yet.
     */
    public RecallOptions scope(String scope) { put("scope", scope); return this; }
    /** Multiple scopes, e.g. ["graph","session"]. */
    public RecallOptions scope(List<String> scopes) { put("scope", scopes); return this; }
    /** As {@link SearchOptions#neighborhoodDepth}. */
    public RecallOptions neighborhoodDepth(int n) { put("neighborhoodDepth", n); return this; }
    /** As {@link SearchOptions#neighborhoodSeedTopK}. */
    public RecallOptions neighborhoodSeedTopK(int n) { put("neighborhoodSeedTopK", n); return this; }
    /** As {@link SearchOptions#includeReferences}; evidence lands in each result's {@code metadata.evidence}. */
    public RecallOptions includeReferences(boolean enabled) { put("includeReferences", enabled); return this; }
    /**
     * The profile the "session_context" scope renders: "qa" (default) or
     * "agent". Not validated: an unknown profile matches no entries.
     */
    public RecallOptions contextProfile(String profile) { put("contextProfile", profile); return this; }
    /**
     * When the "tools" scope runs: "always" (default) or "on_empty". Validated
     * on every call; no effect until that scope exists.
     */
    public RecallOptions toolsTrigger(String trigger) { put("toolsTrigger", trigger); return this; }
    /** What the {@link #nodeName} values are: a graph node class such as "Entity". Omitted, they are node-set tags. */
    public RecallOptions nodeType(String t) { put("nodeType", t); return this; }
    /** Scope the graph source to these node names. */
    public RecallOptions nodeName(List<String> n) { put("nodeName", n); return this; }
    /** How {@link #nodeName} combines: "OR" (default) or "AND". */
    public RecallOptions nodeNameFilterOperator(String op) { put("nodeNameFilterOperator", op); return this; }
    /** Keep the graph search's context and graph in its response. */
    public RecallOptions verbose(boolean b) { put("verbose", b); return this; }
}
