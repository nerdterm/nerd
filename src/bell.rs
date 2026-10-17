//! The loop's doorbell: producers ring after pushing, the loop takes the ring before draining, so no wake is lost.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::Duration;

#[derive(Clone)]
pub struct Bell(SyncSender<()>);

pub struct Door(Receiver<()>);

pub fn new() -> (Bell, Door) {
    let (tx, rx) = sync_channel(1);
    (Bell(tx), Door(rx))
}

impl Bell {
    pub fn ring(&self) {
        let _ = self.0.try_send(());
    }
}

impl Door {
    /// Takes the ring, or waits up to `t` for one; call it before draining, never after.
    pub fn wait(&self, t: Duration) {
        let _ = self.0.recv_timeout(t);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn a_ring_during_the_drain_wakes_the_next_wait() {
        let (bell, door) = super::new();
        let long = Duration::from_secs(5);
        bell.ring();
        bell.ring();
        let t = Instant::now();
        door.wait(long);
        assert!(t.elapsed() < Duration::from_secs(1), "a ring before the wait is taken at once");
        // The loop is draining now; a producer pushes and rings.
        bell.ring();
        let t = Instant::now();
        door.wait(long);
        assert!(t.elapsed() < Duration::from_secs(1), "the ring made during the drain is not lost");
        let t = Instant::now();
        door.wait(Duration::from_millis(30));
        assert!(t.elapsed() >= Duration::from_millis(30), "two rings before a wait coalesce into one");
    }
}
