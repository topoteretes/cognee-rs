package ai.cognee;

/**
 * Unchecked exception carrying a stable machine-readable {@code code()} shared
 * with the other cognee bindings (JS {@code e.code}, C {@code CgErrorCode}).
 * The code string is the contract; branch on it, not on the message.
 */
public class CogneeException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    private final String code;

    public CogneeException(String code, String message) {
        super(message);
        this.code = code;
    }

    /** Stable machine-readable error code (e.g. {@code "VALIDATION_ERROR"}). */
    public String code() {
        return code;
    }
}
