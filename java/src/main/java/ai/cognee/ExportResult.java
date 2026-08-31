package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Result of {@link Cognee#exportCogx}: where the COGX archive landed and what
 * went into it.
 *
 * @param archive   the packed {@code .cogx.tar.gz} tarball — the file Python
 *                  cognee re-imports
 * @param directory the unpacked archive directory the tarball was built from
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ExportResult(
        @JsonProperty("archive") String archive,
        @JsonProperty("directory") String directory,
        @JsonProperty("numNodes") int numNodes,
        @JsonProperty("numEdges") int numEdges,
        @JsonProperty("numEntities") int numEntities,
        @JsonProperty("numDocuments") int numDocuments,
        @JsonProperty("numFacts") int numFacts,
        @JsonProperty("numRawNodes") int numRawNodes) {}
