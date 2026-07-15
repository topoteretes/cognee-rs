package ai.cognee;

import java.util.List;

/** Per-call options for {@link Cognee#search}. */
public final class SearchOptions extends Options {
    public SearchOptions searchType(SearchType t) { if (t != null) put("searchType", t.wire()); return this; }
    public SearchOptions datasets(List<String> d) { put("datasets", d); return this; }
    public SearchOptions datasetIds(List<String> ids) { put("datasetIds", ids); return this; }
    public SearchOptions topK(int n) { put("topK", n); return this; }
    public SearchOptions systemPrompt(String p) { put("systemPrompt", p); return this; }
    public SearchOptions sessionId(String s) { put("sessionId", s); return this; }
    public SearchOptions nodeType(String t) { put("nodeType", t); return this; }
    public SearchOptions nodeName(List<String> n) { put("nodeName", n); return this; }
    public SearchOptions onlyContext(boolean b) { put("onlyContext", b); return this; }
    public SearchOptions useCombinedContext(boolean b) { put("useCombinedContext", b); return this; }
    public SearchOptions verbose(boolean b) { put("verbose", b); return this; }
    public SearchOptions saveInteraction(boolean b) { put("saveInteraction", b); return this; }
    public SearchOptions autoFeedbackDetection(boolean b) { put("autoFeedbackDetection", b); return this; }
}
