use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadMetaInfo {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

impl Default for PayloadMetaInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl PayloadMetaInfo {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadBase {
    pub metainfo: PayloadMetaInfo,
}

impl Default for PayloadBase {
    fn default() -> Self {
        Self::new()
    }
}

impl PayloadBase {
    pub fn new() -> Self {
        Self {
            metainfo: PayloadMetaInfo::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use serde_json;

    #[test]
    fn payload_metainfo_new_initial_values() {
        let meta = PayloadMetaInfo::new();

        // id is a valid v4 UUID (Uuid::new_v4() always yields V4)
        assert_eq!(meta.id.get_version_num(), 4);

        // created_at is close to "now"
        let now = Utc::now();
        assert!(
            meta.created_at <= now,
            "created_at should not be in the future"
        );
        assert!(
            meta.created_at >= now - Duration::seconds(5),
            "created_at is too old: {} vs now {}",
            meta.created_at,
            now
        );
    }

    #[test]
    fn payload_base_new_initializes_metainfo() {
        let base = PayloadBase::new();
        // Ensure metainfo exists and has sane defaults
        assert_eq!(base.metainfo.id.get_version_num(), 4);
    }

    #[test]
    fn default_impls_match_new() {
        let meta_default = PayloadMetaInfo::default();
        let meta_new = PayloadMetaInfo::new();

        // We can't expect IDs/timestamps to match, but we can expect semantics:
        assert_eq!(meta_default.id.get_version_num(), 4);
        assert_eq!(meta_new.id.get_version_num(), 4);

        let base_default = PayloadBase::default();
        let base_new = PayloadBase::new();

        // Same as above: both should have valid metainfo with v4 UUIDs
        for b in [&base_default, &base_new] {
            assert_eq!(b.metainfo.id.get_version_num(), 4);
        }
    }

    #[test]
    fn serde_roundtrip_payload_metainfo() {
        let meta = PayloadMetaInfo::new();

        let json = serde_json::to_string(&meta).expect("serialize");
        let de: PayloadMetaInfo = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(de.id, meta.id);
        assert_eq!(de.created_at, meta.created_at);
    }

    #[test]
    fn serde_roundtrip_payload_base() {
        let base = PayloadBase::new();

        let json = serde_json::to_string(&base).expect("serialize");
        let de: PayloadBase = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(de.metainfo.id, base.metainfo.id);
        assert_eq!(de.metainfo.created_at, base.metainfo.created_at);
    }

    #[test]
    fn clone_and_debug_work() {
        let base = PayloadBase::new();
        let cloned = base.clone();

        // Cloned value should be equal by field values (derive(Clone)).
        assert_eq!(cloned.metainfo.id, base.metainfo.id);
        assert_eq!(cloned.metainfo.created_at, base.metainfo.created_at);

        // Debug shouldn't panic and should contain type name hints.
        let dbg_str = format!("{base:?}");
        assert!(dbg_str.contains("PayloadBase"));
        assert!(dbg_str.contains("PayloadMetaInfo"));
    }

    #[test]
    fn unique_ids_across_instances() {
        // It's extremely likely two fresh instances have different IDs.
        // This guards against accidental reuse/copy.
        let a = PayloadMetaInfo::new();
        let b = PayloadMetaInfo::new();

        assert_ne!(a.id, b.id, "new() should generate fresh UUIDs");
    }
}
