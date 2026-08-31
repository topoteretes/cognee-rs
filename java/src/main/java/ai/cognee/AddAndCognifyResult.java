package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** Result of {@link Cognee#addAndCognify}: the add and cognify results combined. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddAndCognifyResult(@JsonProperty("add") AddResult add,
        @JsonProperty("cognify") CognifyResult cognify) {}
