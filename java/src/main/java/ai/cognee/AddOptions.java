package ai.cognee;

public final class AddOptions extends Options {
    public AddOptions tenant(String tenant) {
        put("tenant", tenant);
        return this;
    }
}
