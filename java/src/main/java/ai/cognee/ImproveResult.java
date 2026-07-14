package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import java.util.List;

@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveResult(
        List<String> stagesRun,
        MemifyResult memifyResult,
        long feedbackEntriesProcessed,
        long feedbackEntriesApplied,
        long sessionsPersisted,
        long edgesSynced) {}
