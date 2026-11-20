package ai.cognee;

/** Per-call options for {@link Cognee#remember}. */
public final class RememberOptions extends Options {
    public RememberOptions sessionId(String s) { put("sessionId", s); return this; }
    /** Run {@code improve()} after the remember; defaults to {@code true}, as in Python. */
    public RememberOptions selfImprovement(boolean b) { put("selfImprovement", b); return this; }
    public RememberOptions tenant(String t) { put("tenant", t); return this; }
    /** Graph extractor for the cognify step, as {@link CognifyOptions#extractor}. Not with {@link #sessionId}. */
    public RememberOptions extractor(String kind) { put("extractor", kind); return this; }
    /**
     * {@code "code"} indexes each input, a local code repository path, as a code graph
     * with no LLM (Python {@code remember(content_type="code")}). Not with {@link #sessionId}.
     */
    public RememberOptions contentType(String contentType) { put("contentType", contentType); return this; }
    /** With {@code contentType("code")}: {@code false} records a failed repository on its item instead of failing. */
    public RememberOptions raiseOnError(boolean b) { put("raiseOnError", b); return this; }
}
