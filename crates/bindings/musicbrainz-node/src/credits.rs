//! Flow control between a producer thread and the JS consumer.
//!
//! The producer may only send a batch while it holds a credit; JS grants
//! credits as it consumes batches (`request`). So at most the granted
//! number of batches is ever queued, however slow the consumer is, and the
//! producer waits on its own thread — never on the event loop or the
//! libuv pool.

use std::sync::{Condvar, Mutex};

#[derive(Default)]
struct State {
    credits: u32,
    closed: bool,
}

/// Credits granted by JS (see the module docs).
#[derive(Default)]
pub(crate) struct Credits {
    state: Mutex<State>,
    cv: Condvar,
}

impl Credits {
    /// Grant `n` credits.
    pub(crate) fn grant(&self, n: u32) {
        let mut s = self.state.lock().unwrap();
        s.credits = s.credits.saturating_add(n);
        self.cv.notify_all();
    }

    /// Stop the producer.
    pub(crate) fn close(&self) {
        let mut s = self.state.lock().unwrap();
        s.closed = true;
        self.cv.notify_all();
    }

    /// Wait for a credit and take it; `false` once closed.
    pub(crate) fn acquire(&self) -> bool {
        let mut s = self.state.lock().unwrap();
        loop {
            if s.closed {
                return false;
            }
            if s.credits > 0 {
                s.credits -= 1;
                return true;
            }
            s = self.cv.wait(s).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn acquire_waits_for_grant_or_close() {
        let c = Arc::new(Credits::default());
        let c2 = c.clone();
        let t = std::thread::spawn(move || (c2.acquire(), c2.acquire()));
        std::thread::sleep(std::time::Duration::from_millis(20));
        c.grant(1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        c.close();
        assert_eq!(t.join().unwrap(), (true, false));
    }
}
