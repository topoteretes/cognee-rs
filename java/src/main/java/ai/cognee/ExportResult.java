package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

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
        String archive,
        String directory,
        int numNodes,
        int numEdges,
        int numEntities,
        int numDocuments,
        int numFacts,
        int numRawNodes) {}
