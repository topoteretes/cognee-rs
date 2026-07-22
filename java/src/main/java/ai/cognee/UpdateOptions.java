package ai.cognee;

import java.util.List;
import java.util.Map;

/** Per-call options for {@link Cognee#update}. */
public final class UpdateOptions extends Options {
    public UpdateOptions datasetId(String id) { put("datasetId", id); return this; }
    public UpdateOptions tenant(String t) { put("tenant", t); return this; }
    public UpdateOptions nodeSet(List<String> s) { put("nodeSet", s); return this; }
    public UpdateOptions preferredLoaders(Map<String, String> m) { put("preferredLoaders", m); return this; }
    public UpdateOptions incrementalLoading(boolean b) { put("incrementalLoading", b); return this; }
}
