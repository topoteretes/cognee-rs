package ai.cognee;

public final class PruneSystemOptions extends Options {
    public PruneSystemOptions pruneGraph(boolean b) { put("pruneGraph", b); return this; }
    public PruneSystemOptions pruneVector(boolean b) { put("pruneVector", b); return this; }
    public PruneSystemOptions pruneMetadata(boolean b) { put("pruneMetadata", b); return this; }
    public PruneSystemOptions pruneCache(boolean b) { put("pruneCache", b); return this; }
}
