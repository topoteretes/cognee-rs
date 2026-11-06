package ai.cognee;

/** Options for {@link Cognee#waitForBackgroundTasks(WaitForBackgroundTasksOptions)}. */
public final class WaitForBackgroundTasksOptions extends Options {
    /**
     * Bound the wait, in seconds (non-negative); nothing is cancelled when it elapses. A negative
     * value fails the future with a {@code VALIDATION_ERROR}.
     */
    public WaitForBackgroundTasksOptions timeoutSeconds(double seconds) {
        put("timeoutSeconds", seconds);
        return this;
    }
}
