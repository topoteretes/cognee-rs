package ai.cognee;

/**
 * Per-call options for {@link Cognee#exportCogx}.
 *
 * <p>{@code datasetName} is a manifest <em>label</em> only. It does not scope
 * the export: cognee-rs has no per-dataset graph partition, so the archive
 * always contains the whole graph store.
 */
public final class ExportOptions extends Options {
    public ExportOptions embeddingModel(String m) { put("embeddingModel", m); return this; }
    public ExportOptions datasetName(String n) { put("datasetName", n); return this; }
}
