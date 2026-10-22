package ai.cognee;

import java.util.List;
import java.util.Map;

/** Per-call options for {@link Cognee#search}. */
public final class SearchOptions extends Options {
    public SearchOptions searchType(SearchType t) { if (t != null) put("searchType", t.wire()); return this; }
    public SearchOptions datasets(List<String> d) { put("datasets", d); return this; }
    public SearchOptions datasetIds(List<String> ids) { put("datasetIds", ids); return this; }
    /**
     * Tenant UUID, matching the {@code tenant} passed to {@code add}/{@code cognify}.
     * Scopes {@link #datasets} name resolution: one handle can hold same-named
     * datasets in several tenants. Omit for the single-tenant default.
     */
    public SearchOptions tenant(String tenant) { put("tenant", tenant); return this; }
    public SearchOptions topK(int n) { put("topK", n); return this; }
    public SearchOptions systemPrompt(String p) { put("systemPrompt", p); return this; }
    public SearchOptions sessionId(String s) { put("sessionId", s); return this; }
    public SearchOptions nodeType(String t) { put("nodeType", t); return this; }
    public SearchOptions nodeName(List<String> n) { put("nodeName", n); return this; }
    public SearchOptions nodeNameFilterOperator(String op) { put("nodeNameFilterOperator", op); return this; }
    public SearchOptions onlyContext(boolean b) { put("onlyContext", b); return this; }
    public SearchOptions useCombinedContext(boolean b) { put("useCombinedContext", b); return this; }
    public SearchOptions verbose(boolean b) { put("verbose", b); return this; }
    public SearchOptions autoFeedbackDetection(boolean b) { put("autoFeedbackDetection", b); return this; }
    /**
     * Graph searches only: rank the triplets within this many hops of the
     * nearest vector-search hits, not just the edges touching them. Must be
     * positive; larger values load more of the graph.
     */
    public SearchOptions neighborhoodDepth(int n) { put("neighborhoodDepth", n); return this; }
    /**
     * With {@link #neighborhoodDepth}: expand from at most this many hits,
     * nearest first (all of them when unset). Must be positive.
     */
    public SearchOptions neighborhoodSeedTopK(int n) { put("neighborhoodSeedTopK", n); return this; }
    /**
     * Per-retriever knobs, forwarded verbatim to the retriever the search
     * resolves to. Keys are snake_case and retriever-defined; a key the chosen
     * retriever does not know is ignored, not an error.
     *
     * <p>The hybrid retriever reads {@code text_summaries_top_k} here — how
     * many {@code TextSummary} candidates its chunk lane fuses against the
     * {@code DocumentChunk} lane, defaulting to the chunk lane's own top-k.
     *
     * @param c snake_case knob names to JSON-serializable values
     */
    public SearchOptions retrieverSpecificConfig(Map<String, Object> c) { put("retrieverSpecificConfig", c); return this; }
}
