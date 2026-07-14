package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record CognifyResult(
        int chunks,
        int entities,
        int edges,
        int summaries,
        int embeddings,
        boolean alreadyCompleted,
        String priorPipelineRunId) {}
