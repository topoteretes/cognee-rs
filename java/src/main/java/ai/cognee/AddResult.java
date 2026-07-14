package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import java.util.List;

/** Result of {@link Cognee#add}: which items were added versus deduplicated. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddResult(
        String datasetName,
        List<CogneeData> added,
        int addedCount,
        List<CogneeData> deduplicated,
        int deduplicatedCount) {}
