package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Result of {@link Cognee#improve}: one {@link ImproveStageResult} per improve stage, in registry
 * order, and the run's summary {@code status} ({@code completed}, {@code errored}, {@code skipped}
 * or {@code running}).
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveResult(
        @JsonProperty("datasetId") String datasetId,
        @JsonProperty("datasetName") String datasetName,
        @JsonProperty("sessionIds") List<String> sessionIds,
        @JsonProperty("stages") List<ImproveStageResult> stages,
        @JsonProperty("background") boolean background,
        @JsonProperty("finished") boolean finished,
        @JsonProperty("error") String error,
        @JsonProperty("status") String status) {}
