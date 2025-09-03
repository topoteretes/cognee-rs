use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadMetaInfo {
    pub id: Uuid,
    pub task_counter: u32,
    pub created_at: DateTime<Utc>,
}

impl PayloadMetaInfo {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            task_counter: 0,
            created_at: Utc::now(),
        }
    }
    pub fn task_done(&mut self) {
        self.task_counter += 1;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadBase {
    pub metainfo: PayloadMetaInfo,
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
    use crate::data::payload_types::cognee_payload::CogneePayload;
    use crate::data::payload_types::low_level_payload::LowLevelPayload;
    use chrono::Utc;
    use serde_json;

    #[test]
    fn constructs_and_increments_counter() {
        let before = Utc::now();
        let mut p = PayloadBase::new();
        let after = Utc::now();

        assert_eq!(p.metainfo.task_counter, 0);
        p.metainfo.task_done();
        assert_eq!(p.metainfo.task_counter, 1);
        assert!(!p.metainfo.id.is_nil());

        // Verify created_at is within reasonable bounds
        assert!(p.metainfo.created_at >= before);
        assert!(p.metainfo.created_at <= after);
    }

    #[test]
    fn serde_roundtrip() {
        let mut p = PayloadBase::new();
        // Increment counter a few times to test serialization
        p.metainfo.task_done();
        p.metainfo.task_done();
        p.metainfo.task_done();

        let json = serde_json::to_string(&p).unwrap();
        let back: PayloadBase = serde_json::from_str(&json).unwrap();
        assert_eq!(back.metainfo.task_counter, 3);
    }
    use crate::data::traits::PayloadBehavior;
    #[test]
    fn test() {
        let mut items: Vec<Box<dyn PayloadBehavior>> = vec![
            Box::new(CogneePayload::new(vec![
                "hello world".into(),
                "lorem ipsum".into(),
            ])),
            Box::new(LowLevelPayload::new(
                1920,
                1080,
                vec!["tile_a".into(), "tile_b".into()],
            )),
        ];

        for p in items.iter_mut() {
            println!("id={}", p.id());
            p.task_done();
        }
    }
}
