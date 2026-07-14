package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** Result of {@link Cognee#pruneSystem}: which backends were pruned. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PruneResult(
        boolean dataPruned,
        boolean graphPruned,
        boolean vectorPruned,
        boolean metadataPruned,
        boolean cachePruned) {}
