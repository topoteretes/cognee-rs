package ai.cognee;

/** Per-call options for {@link Cognee#remember}. */
public final class RememberOptions extends Options {
    public RememberOptions sessionId(String s) { put("sessionId", s); return this; }
    public RememberOptions selfImprovement(boolean b) { put("selfImprovement", b); return this; }
    public RememberOptions tenant(String t) { put("tenant", t); return this; }
}
