package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** Result of {@link Cognee#pruneSystem}: which backends were pruned. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PruneResult(
        @JsonProperty("dataPruned") boolean dataPruned,
        @JsonProperty("graphPruned") boolean graphPruned,
        @JsonProperty("vectorPruned") boolean vectorPruned,
        @JsonProperty("metadataPruned") boolean metadataPruned,
        @JsonProperty("cachePruned") boolean cachePruned) {}
