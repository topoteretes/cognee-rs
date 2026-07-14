package ai.cognee.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class NativeLoadTest {
    @Test
    void libraryLoadsAndVersionsMatch() {
        // Class-load of Native runs the handshake; reaching version() means it passed.
        String v = Native.version();
        assertNotNull(v);
        assertEquals(NativeLibLoader.jarVersion(), v);
    }
}
