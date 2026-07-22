package ai.cognee;

/** Per-call options for {@link Cognee#pruneSystem} (which backends to wipe). */
public final class PruneSystemOptions extends Options {
    public PruneSystemOptions pruneGraph(boolean b) { put("pruneGraph", b); return this; }
    public PruneSystemOptions pruneVector(boolean b) { put("pruneVector", b); return this; }
    public PruneSystemOptions pruneMetadata(boolean b) { put("pruneMetadata", b); return this; }
    public PruneSystemOptions pruneCache(boolean b) { put("pruneCache", b); return this; }
}
