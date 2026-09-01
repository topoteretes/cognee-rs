package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/** Result of {@link Cognee#improve}: which stages ran and what they applied. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveResult(
        @JsonProperty("stagesRun") List<String> stagesRun,
        @JsonProperty("memifyResult") MemifyResult memifyResult,
        @JsonProperty("feedbackEntriesProcessed") long feedbackEntriesProcessed,
        @JsonProperty("feedbackEntriesApplied") long feedbackEntriesApplied,
        @JsonProperty("sessionsPersisted") long sessionsPersisted,
        @JsonProperty("edgesSynced") long edgesSynced) {}
