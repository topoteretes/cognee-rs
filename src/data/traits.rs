use uuid::Uuid;

pub trait PayloadBehavior {
    fn id(&self) -> Uuid;
    fn task_counter(&self) -> u32;
    fn task_done(&mut self);
    fn chunks(&self) -> &[String];
}