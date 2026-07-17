use tokio::sync::mpsc;

use crate::traits::Trigger;

pub struct MockTrigger(pub mpsc::Receiver<()>);

impl Trigger for MockTrigger {
    async fn wait_untill_next_edge(&mut self) {
        self.0.recv().await;
    }

    async fn wait_untill_any_edge(&mut self) {
        self.0.recv().await;
    }

    fn poll_ready(&mut self) -> bool {
        self.0.try_recv().is_ok()
    }
}
