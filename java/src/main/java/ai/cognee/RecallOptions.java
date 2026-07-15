package ai.cognee;

import java.util.List;

/** Per-call options for {@link Cognee#recall}. */
public final class RecallOptions extends Options {
    public RecallOptions searchType(SearchType t) { if (t != null) put("searchType", t.wire()); return this; }
    public RecallOptions datasets(List<String> d) { put("datasets", d); return this; }
    public RecallOptions topK(int n) { put("topK", n); return this; }
    public RecallOptions autoRoute(boolean b) { put("autoRoute", b); return this; }
    public RecallOptions sessionId(String s) { put("sessionId", s); return this; }
    /** A single scope, e.g. "graph". */
    public RecallOptions scope(String scope) { put("scope", scope); return this; }
    /** Multiple scopes, e.g. ["graph","session"]. */
    public RecallOptions scope(List<String> scopes) { put("scope", scopes); return this; }
}
