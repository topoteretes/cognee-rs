package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record PruneResult(
        boolean dataPruned,
        boolean graphPruned,
        boolean vectorPruned,
        boolean metadataPruned,
        boolean cachePruned) {}
