package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Result of {@link Cognee#waitForBackgroundTasks}: {@code finished} is {@code false} only when the
 * timeout elapsed with work still running; {@code pending} counts the tasks still running.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record BackgroundTasksResult(
        @JsonProperty("finished") boolean finished,
        @JsonProperty("pending") long pending) {}
