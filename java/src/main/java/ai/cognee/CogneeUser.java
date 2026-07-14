package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** A user ({@code id} + {@code email}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeUser(String id, String email) {}
