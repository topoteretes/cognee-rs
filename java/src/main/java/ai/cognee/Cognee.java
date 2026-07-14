package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import java.lang.ref.Cleaner;
import java.util.Map;

/**
 * The cognee Java SDK entry point. Construct with optional settings (canonical
 * snake_case {@code Settings} field names), then drive the pipeline. Holds a
 * native handle; call {@link #close()} to release it (a {@link Cleaner} is a
 * leak backstop, but {@code close()} is the primary path).
 */
public final class Cognee implements AutoCloseable {
    private static final Cleaner CLEANER = Cleaner.create();

    /** Mutable holder so the Cleaner can null the handle after freeing it. */
    private static final class Handle implements Runnable {
        private long ptr;

        Handle(long ptr) {
            this.ptr = ptr;
        }

        @Override
        public void run() {
            if (ptr != 0) {
                Native.destroy(ptr);
                ptr = 0;
            }
        }
    }

    private final Handle handleHolder;
    private final Cleaner.Cleanable cleanable;
    private volatile boolean closed = false;

    /** Construct from environment/default settings. */
    public Cognee() {
        this((String) null);
    }

    /** Construct from a settings map (canonical snake_case keys). */
    public Cognee(Map<String, ?> settings) {
        this(settings == null ? null : Json.toJson(settings));
    }

    /** Construct from a settings JSON string (or {@code null} for env-only). */
    public Cognee(String settingsJson) {
        long ptr = Native.newHandle(settingsJson); // throws CogneeException on bad settings
        this.handleHolder = new Handle(ptr);
        this.cleanable = CLEANER.register(this, this.handleHolder);
    }

    /** The native handle for internal op calls. Throws if closed. */
    public long handle() {
        if (closed) {
            throw new IllegalStateException("Cognee handle is closed");
        }
        return handleHolder.ptr;
    }

    @Override
    public void close() {
        if (closed) {
            return;
        }
        closed = true;
        cleanable.clean(); // runs Handle.run() exactly once → Native.destroy
    }
}
