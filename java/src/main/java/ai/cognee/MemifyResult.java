package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** Result of {@link Cognee#memify}: triplet indexing counts. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record MemifyResult(
        @JsonProperty("tripletCount") long tripletCount,
        @JsonProperty("indexedCount") long indexedCount,
        @JsonProperty("batchCount") long batchCount,
        @JsonProperty("alreadyCompleted") boolean alreadyCompleted,
        @JsonProperty("priorPipelineRunId") String priorPipelineRunId) {}
