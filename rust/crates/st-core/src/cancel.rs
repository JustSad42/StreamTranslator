//! Cancellation flags. A job can be cancelled by the user; the background video download also
//! stops when the job ends early (`either`).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Debug)]
pub struct Cancel(Vec<Arc<AtomicBool>>);

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancel {
    pub fn new() -> Self {
        Cancel(vec![Arc::new(AtomicBool::new(false))])
    }
    pub fn set(&self) {
        if let Some(f) = self.0.first() {
            f.store(true, Ordering::SeqCst);
        }
    }
    pub fn is_set(&self) -> bool {
        self.0.iter().any(|f| f.load(Ordering::SeqCst))
    }
    /// Cancelled when either is.
    pub fn either(&self, other: &Cancel) -> Cancel {
        let mut v = self.0.clone();
        v.extend(other.0.iter().cloned());
        Cancel(v)
    }
    /// Sleep up to `secs`, waking early when cancelled. True if cancelled (Python's Event.wait).
    pub fn wait(&self, secs: f64) -> bool {
        let end = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        while !self.is_set() {
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(std::time::Duration::from_millis(50)));
        }
        self.is_set()
    }
    pub fn check(&self) -> anyhow::Result<()> {
        if self.is_set() {
            Err(crate::Cancelled.into())
        } else {
            Ok(())
        }
    }
}
