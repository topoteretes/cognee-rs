use crate::data::traits::PayloadBehavior;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadMetaInfo {
    pub id: Uuid,
    pub task_counter: u32,
}

impl PayloadMetaInfo {
    pub fn new(task_counter: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            task_counter,
        }
    }
    pub fn task_done(&mut self) {
        self.task_counter += 1;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadBase<T> {
    pub metainfo: PayloadMetaInfo,
    pub data: T,
}

impl<T> PayloadBase<T> {
    pub fn new(task_counter: u32, data: T) -> Self {
        Self {
            metainfo: PayloadMetaInfo::new(task_counter),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::payload_types::cognee_payload::CogneePayload;
    use crate::data::payload_types::low_level_payload::LowLevelPayload;
    use serde_json;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct DataPoint {
        path: String,
        chunks: Vec<String>,
    }

    #[test]
    fn constructs_and_increments_counter() {
        let dp = DataPoint {
            path: "a".into(),
            chunks: vec!["x".into()],
        };
        let mut p = PayloadBase::new(0, dp);
        assert_eq!(p.metainfo.task_counter, 0);
        p.metainfo.task_done();
        assert_eq!(p.metainfo.task_counter, 1);
        assert!(!p.metainfo.id.is_nil());
        assert_eq!(p.data.path, "a");
    }

    #[test]
    fn serde_roundtrip() {
        let dp = DataPoint {
            path: "a".into(),
            chunks: vec!["x".into(), "y".into()],
        };
        let p = PayloadBase::new(3, dp);
        let json = serde_json::to_string(&p).unwrap();
        let back: PayloadBase<DataPoint> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.metainfo.task_counter, 3);
        assert_eq!(back.data.chunks, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn test() {
        let mut items: Vec<Box<dyn PayloadBehavior>> = vec![
            Box::new(CogneePayload::new(
                0,
                "Intro",
                vec!["hello world".into(), "lorem ipsum".into()],
            )),
            Box::new(LowLevelPayload::new(
                2,
                1920,
                1080,
                vec!["tile_a".into(), "tile_b".into()],
            )),
        ];

        for p in items.iter_mut() {
            println!(
                "id={} counter={} first_chunk={:?}",
                p.id(),
                p.task_counter(),
                p.chunks().first(),
            );
            p.task_done();
        }
    }
}
