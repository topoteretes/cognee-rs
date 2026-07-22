package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** Result of {@link Cognee#memify}: triplet indexing counts. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record MemifyResult(
        long tripletCount,
        long indexedCount,
        long batchCount,
        boolean alreadyCompleted,
        String priorPipelineRunId) {}
