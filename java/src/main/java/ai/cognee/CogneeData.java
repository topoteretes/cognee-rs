package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A single ingested data item.
 *
 * @param id the item id
 * @param name the item name
 * @param tokenCount the token count of the item's text, or {@code null} until
 *     cognify has run on the item; {@link Cognee#addAndCognify} returns the
 *     count cognify computed (still {@code null} for an item cognify could
 *     not count)
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeData(@JsonProperty("id") String id,
        @JsonProperty("name") String name,
        @JsonProperty("token_count") Long tokenCount) {}
