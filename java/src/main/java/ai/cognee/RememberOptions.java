package ai.cognee;

/** Per-call options for {@link Cognee#remember}. */
public final class RememberOptions extends Options {
    public RememberOptions sessionId(String s) { put("sessionId", s); return this; }
    /** Run {@code improve()} after the remember; defaults to {@code true}, as in Python. */
    public RememberOptions selfImprovement(boolean b) { put("selfImprovement", b); return this; }
    public RememberOptions tenant(String t) { put("tenant", t); return this; }
    /** Graph extractor for the cognify step, as {@link CognifyOptions#extractor}. Not with {@link #sessionId}. */
    public RememberOptions extractor(String kind) { put("extractor", kind); return this; }
}
