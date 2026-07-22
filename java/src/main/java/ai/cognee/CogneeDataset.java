package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** A dataset ({@code id} + {@code name}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeDataset(String id, String name) {}
