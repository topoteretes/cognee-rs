package ai.cognee;

/** Per-call options for {@link Cognee#add}. */
public final class AddOptions extends Options {
    public AddOptions tenant(String tenant) {
        put("tenant", tenant);
        return this;
    }
}
