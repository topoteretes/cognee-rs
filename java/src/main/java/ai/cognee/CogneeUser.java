package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/** A user ({@code id} + {@code email}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeUser(@JsonProperty("id") String id,
        @JsonProperty("email") String email) {}
