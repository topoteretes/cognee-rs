package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** Result of {@link Cognee#cognify}: counts of extracted graph elements. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CognifyResult(
        @JsonProperty("chunks") int chunks,
        @JsonProperty("entities") int entities,
        @JsonProperty("edges") int edges,
        @JsonProperty("summaries") int summaries,
        @JsonProperty("embeddings") int embeddings,
        @JsonProperty("alreadyCompleted") boolean alreadyCompleted,
        @JsonProperty("priorPipelineRunId") String priorPipelineRunId) {}
