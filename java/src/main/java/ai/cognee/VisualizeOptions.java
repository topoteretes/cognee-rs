package ai.cognee;

public final class VisualizeOptions extends Options {
    /** Output path for {@code visualizeToFile} (ignored by {@code visualize}). */
    public VisualizeOptions destinationPath(String path) { put("destinationPath", path); return this; }
}
