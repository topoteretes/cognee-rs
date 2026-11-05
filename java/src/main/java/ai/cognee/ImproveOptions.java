package ai.cognee;

import java.util.List;

/** {@code datasetName} is required and enforced at construction. */
public final class ImproveOptions extends Options {
    public ImproveOptions(String datasetName) {
        if (datasetName == null || datasetName.isEmpty()) {
            throw new IllegalArgumentException("ImproveOptions requires a non-empty datasetName");
        }
        put("datasetName", datasetName);
    }

    /** UUID of a dataset the caller already resolved (possibly shared with it), as Python's {@code improve(dataset=<UUID>)}. */
    public ImproveOptions datasetId(String id) { put("datasetId", id); return this; }
    public ImproveOptions sessionIds(List<String> ids) { put("sessionIds", ids); return this; }
    public ImproveOptions feedbackAlpha(double a) { put("feedbackAlpha", a); return this; }
    public ImproveOptions tenant(String t) { put("tenant", t); return this; }
    /** Opt in to the {@code global_context_index} stage (default false). */
    public ImproveOptions buildGlobalContextIndex(boolean b) { put("buildGlobalContextIndex", b); return this; }
    /** Opt in to the {@code build_truth_subspace} stage (default false). */
    public ImproveOptions buildTruthSubspace(boolean b) { put("buildTruthSubspace", b); return this; }
    /**
     * Return a {@code running} result at once and run the improve as a process-wide
     * background task. No per-run handle.
     */
    public ImproveOptions runInBackground(boolean b) { put("runInBackground", b); return this; }
}
