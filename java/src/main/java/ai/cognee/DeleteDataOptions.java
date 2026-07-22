package ai.cognee;

/** Per-call options for dataset data deletion. */
public final class DeleteDataOptions extends Options {
    public DeleteDataOptions softDelete(boolean b) { put("softDelete", b); return this; }
    public DeleteDataOptions deleteDatasetIfEmpty(boolean b) { put("deleteDatasetIfEmpty", b); return this; }
}
