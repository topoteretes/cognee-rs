package ai.cognee;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.stream.Stream;

/**
 * Waits for SQLite to release its {@code -wal}/{@code -shm} sidecars before a
 * JUnit {@code @TempDir} is torn down.
 *
 * <p>Why this is needed: {@code Cognee.close()} drops the native handle, which
 * drops the sqlx pool — but sqlx's pool {@code Drop} schedules connection
 * teardown onto the runtime rather than closing synchronously, so
 * {@code close()} returns before SQLite has unlinked {@code cognee.db-wal} and
 * {@code cognee.db-shm}. JUnit then starts deleting the temp directory, walks
 * those two files, and they vanish underneath it — surfacing as
 * {@code IOException: Failed to delete temp directory ... cognee.db-shm,
 * cognee.db-wal} with suppressed {@code NoSuchFileException}s. Note the
 * assertions have already passed at that point; the failure is pure teardown,
 * and the end state (files gone) is the desired one.
 *
 * <p>Fixing it at the source would mean threading a blocking shutdown from the
 * JNI {@code destroy} through {@code ComponentManager} to the relational pool,
 * which changes close semantics for all four bindings — disproportionate to a
 * teardown race. Setting a non-WAL journal mode is not available either:
 * {@code crates/database/src/connection.rs} unconditionally applies
 * {@code journal_mode(Wal)} to writable file databases, overriding whatever the
 * URL asked for.
 *
 * <p>Usage — declare it <em>before</em> the {@code Cognee} in the same
 * try-with-resources, so that it closes <em>after</em> it (resources close in
 * reverse order):
 *
 * <pre>{@code
 * try (SqliteSettle settle = SqliteSettle.on(dir);
 *      Cognee cognee = handle(dir)) {
 *     ...
 * }
 * }</pre>
 */
final class SqliteSettle implements AutoCloseable {

    /**
     * Generous ceiling: the wait normally completes in a single-digit number of
     * milliseconds. It exists so a genuine leak fails the test loudly rather
     * than hanging the suite.
     */
    private static final long TIMEOUT_MILLIS = 10_000;

    private static final long POLL_MILLIS = 10;

    private final Path dir;

    private SqliteSettle(Path dir) {
        this.dir = dir;
    }

    static SqliteSettle on(Path dir) {
        return new SqliteSettle(dir);
    }

    /**
     * Deliberately declares no checked exceptions, so adding this to an existing
     * try-with-resources does not force a {@code throws} clause onto every test
     * method that uses it.
     */
    @Override
    public void close() {
        long deadline = System.nanoTime() + TIMEOUT_MILLIS * 1_000_000L;
        while (sidecarsPresent()) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError(
                        "SQLite did not release its -wal/-shm sidecars under "
                                + dir
                                + " within "
                                + TIMEOUT_MILLIS
                                + "ms of Cognee.close(); a connection is likely leaked");
            }
            try {
                Thread.sleep(POLL_MILLIS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new AssertionError("interrupted while waiting for SQLite teardown", e);
            }
        }
    }

    /** True while any {@code *-wal} / {@code *-shm} file remains under {@code dir}. */
    private boolean sidecarsPresent() {
        if (!Files.isDirectory(dir)) {
            return false;
        }
        try (Stream<Path> tree = Files.walk(dir)) {
            return tree.map(Path::getFileName)
                    .filter(java.util.Objects::nonNull)
                    .map(Path::toString)
                    .anyMatch(name -> name.endsWith("-wal") || name.endsWith("-shm"));
        } catch (IOException | java.io.UncheckedIOException e) {
            // The walk races the very deletions being waited on, so a file
            // disappearing mid-walk means progress, not failure. Re-probe
            // rather than breaking the teardown this guard exists to protect.
            return true;
        }
    }
}
