package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.Map;

/** Discriminated target for {@link Cognee#forget}. */
public final class ForgetTarget {
    private final Map<String, Object> fields;

    private ForgetTarget(Map<String, Object> fields) {
        this.fields = fields;
    }

    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    public static ForgetTarget item(String dataId, ForgetTarget.DatasetRef dataset) {
        return new ForgetTarget(Map.of("kind", "item", "dataId", dataId, "dataset", dataset.map));
    }

    public static ForgetTarget dataset(ForgetTarget.DatasetRef dataset) {
        return new ForgetTarget(Map.of("kind", "dataset", "dataset", dataset.map));
    }

    public static ForgetTarget all() {
        return new ForgetTarget(Map.of("kind", "all"));
    }

    /** `{name:…}` or `{id:…}` dataset reference. */
    public static final class DatasetRef {
        final Map<String, Object> map;

        private DatasetRef(Map<String, Object> map) {
            this.map = map;
        }

        public static DatasetRef byName(String name) {
            return new DatasetRef(Map.of("name", name));
        }

        public static DatasetRef byId(String id) {
            return new DatasetRef(Map.of("id", id));
        }
    }
}
