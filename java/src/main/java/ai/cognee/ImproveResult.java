package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import java.util.List;

/** Result of {@link Cognee#improve}: which stages ran and what they applied. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveResult(
        List<String> stagesRun,
        MemifyResult memifyResult,
        long feedbackEntriesProcessed,
        long feedbackEntriesApplied,
        long sessionsPersisted,
        long edgesSynced) {}
