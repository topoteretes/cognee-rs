use uuid::Uuid;

pub trait PayloadBehavior {
    fn id(&self) -> Uuid;
    fn task_done(&mut self);
}
