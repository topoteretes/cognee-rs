package ai.cognee;

/** Per-call options for {@link Cognee#cognify} / {@link Cognee#addAndCognify}. */
public final class CognifyOptions extends Options {
    public CognifyOptions tenant(String tenant) { put("tenant", tenant); return this; }
    public CognifyOptions chunkSize(int n) { put("chunkSize", n); return this; }
    public CognifyOptions chunkOverlap(int n) { put("chunkOverlap", n); return this; }
    public CognifyOptions summarization(boolean b) { put("summarization", b); return this; }
    public CognifyOptions temporalCognify(boolean b) { put("temporalCognify", b); return this; }
    public CognifyOptions triplet(boolean b) { put("triplet", b); return this; }
}
