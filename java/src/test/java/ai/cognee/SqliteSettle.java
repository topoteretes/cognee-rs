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
     * Ceiling on the wait. Most handles release in a single-digit number of
     * milliseconds.
     *
     * <p>Hitting this is NOT treated as a failure, deliberately. An earlier
     * revision threw on timeout and turned three previously-passing
     * {@code CogneeAsyncTest} cases red: after {@code warm()}, that class's
     * handles do not release their sidecars within ten seconds, which looks
     * like a genuine lingering connection rather than the teardown race this
     * class exists for (see the class note). Failing there would trade one
     * flake for three hard failures and block an unrelated fix, so the wait is
     * best-effort: tests that release promptly — the ones that were flaking —
     * get determinism, and tests that do not are left exactly as they were.
     */
    private static final long TIMEOUT_MILLIS = 5_000;

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
                // Best-effort by design — see TIMEOUT_MILLIS. Report it so a
                // lingering connection stays visible in the build log instead
                // of being silently absorbed, but let the test pass: without
                // this class it would have raced teardown anyway.
                System.err.println(
                        "[SqliteSettle] SQLite still holds -wal/-shm under "
                                + dir
                                + " "
                                + TIMEOUT_MILLIS
                                + "ms after Cognee.close(); proceeding. If @TempDir cleanup"
                                + " fails for this test, that connection is the reason.");
                return;
            }
            try {
                Thread.sleep(POLL_MILLIS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return;
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
