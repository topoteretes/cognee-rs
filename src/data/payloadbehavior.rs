use uuid::Uuid;

pub trait PayloadBehavior {
    fn id(&self) -> Uuid;
    fn task_done(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DummyPayload {
        id: Uuid,
        counter: u32,
    }

    impl DummyPayload {
        fn new() -> Self {
            Self {
                id: Uuid::new_v4(),
                counter: 0,
            }
        }

        fn counter(&self) -> u32 {
            self.counter
        }
    }

    impl PayloadBehavior for DummyPayload {
        fn id(&self) -> Uuid {
            self.id
        }
        fn task_done(&mut self) {
            self.counter = self.counter.saturating_add(1);
        }
    }

    #[test]
    fn id_is_stable_for_instance() {
        let p = DummyPayload::new();
        let first = p.id();
        let second = p.id();
        assert_eq!(first, second, "id() should be stable for a given instance");
    }

    #[test]
    fn task_done_increments_counter() {
        let mut p = DummyPayload::new();
        assert_eq!(p.counter(), 0);
        p.task_done();
        assert_eq!(p.counter(), 1);
        p.task_done();
        assert_eq!(p.counter(), 2);
    }

    #[test]
    fn different_instances_have_different_ids() {
        let a = DummyPayload::new();
        let b = DummyPayload::new();
        assert_ne!(
            a.id(),
            b.id(),
            "distinct instances should have distinct ids"
        );
    }
}
