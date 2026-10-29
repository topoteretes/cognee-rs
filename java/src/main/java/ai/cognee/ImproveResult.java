package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Result of {@link Cognee#improve}, Python's {@code ImproveResult}: one {@link ImproveStageResult}
 * per stage, in order ({@code feedback_weights}, {@code persist_session_qa}, {@code
 * persist_agent_traces}, {@code extract_agent_context}, {@code distill_sessions}, {@code
 * update_user_preferences}, {@code build_truth_subspace}, {@code review_conflicts}, {@code
 * global_context_index}), and the run's {@code status} ({@code completed}, {@code errored} or
 * {@code skipped}).
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
