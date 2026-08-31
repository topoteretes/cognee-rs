package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** A single ingested data item ({@code id} + {@code name}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeData(@JsonProperty("id") String id,
        @JsonProperty("name") String name) {}
