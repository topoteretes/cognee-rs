package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** Result of {@link Cognee#cognify}: counts of extracted graph elements. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CognifyResult(
        int chunks,
        int entities,
        int edges,
        int summaries,
        int embeddings,
        boolean alreadyCompleted,
        String priorPipelineRunId) {}
