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
}
