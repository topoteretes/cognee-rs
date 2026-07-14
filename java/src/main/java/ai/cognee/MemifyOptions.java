package ai.cognee;

import java.util.List;

public final class MemifyOptions extends Options {
    public MemifyOptions tripletBatchSize(int n) { put("tripletBatchSize", n); return this; }
    public MemifyOptions nodeTypeFilter(String s) { put("nodeTypeFilter", s); return this; }
    public MemifyOptions nodeNameFilter(List<String> names) { put("nodeNameFilter", names); return this; }
    public MemifyOptions nodeNameFilterOperator(String op) { put("nodeNameFilterOperator", op); return this; }
}
