package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** A dataset ({@code id} + {@code name}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeDataset(@JsonProperty("id") String id,
        @JsonProperty("name") String name) {}
