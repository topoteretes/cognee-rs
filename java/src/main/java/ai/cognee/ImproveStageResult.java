package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.Map;

/**
 * What one {@link Cognee#improve} stage did: its status ({@code completed}, {@code
 * already_completed}, {@code skipped} or {@code errored}), why it was skipped or what failed, the
 * counts it reported and how long it ran.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveStageResult(
        @JsonProperty("stage") String stage,
        @JsonProperty("status") String status,
        @JsonProperty("reason") String reason,
        @JsonProperty("error") String error,
        @JsonProperty("counts") Map<String, Long> counts,
        @JsonProperty("durationMs") long durationMs) {}
