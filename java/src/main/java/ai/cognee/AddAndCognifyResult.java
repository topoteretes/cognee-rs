package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** Result of {@link Cognee#addAndCognify}: the add and cognify results combined. */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddAndCognifyResult(AddResult add, CognifyResult cognify) {}
