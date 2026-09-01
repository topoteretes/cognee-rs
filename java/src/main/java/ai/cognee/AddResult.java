package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/** Result of {@link Cognee#add}: which items were added versus deduplicated. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddResult(
        @JsonProperty("datasetName") String datasetName,
        @JsonProperty("added") List<CogneeData> added,
        @JsonProperty("addedCount") int addedCount,
        @JsonProperty("deduplicated") List<CogneeData> deduplicated,
        @JsonProperty("deduplicatedCount") int deduplicatedCount) {}
