package ai.cognee;

/** Per-call options for {@link Cognee#visualize} / {@link Cognee#visualizeToFile}. */
public final class VisualizeOptions extends Options {
    /** Output path for {@code visualizeToFile} (ignored by {@code visualize}). */
    public VisualizeOptions destinationPath(String path) { put("destinationPath", path); return this; }
}
