package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

/** A single ingested data item ({@code id} + {@code name}). */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeData(String id, String name) {}
