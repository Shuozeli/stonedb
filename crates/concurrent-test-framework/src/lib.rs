//! Concurrent Test Framework
//!
//! A framework for testing concurrent code with controlled interleaving
//! and dynamic capture.
//!
//! In production builds (without `cfg(test)`), checkpoints are no-ops.
//! In test builds, checkpoints intercept values and block threads until released.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::thread;

/// Decision made by hook when thread reaches checkpoint
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Capture value and block thread until released
    CaptureAndBlock,
    /// Block thread without capturing
    BlockWithoutCapture,
    /// Proceed without blocking or capturing
    Proceed,
}

/// Hook type - called with debug-formatted value, returns Decision
pub type Hook = Box<dyn FnMut(String) -> Decision + Send>;

/// Per-checkpoint blocking state
struct BlockingState {
    released: AtomicBool,
    waiting: Mutex<usize>,
    #[allow(dead_code)]
    timeout_ms: u64,
}

impl BlockingState {
    fn new(timeout_ms: u64) -> Self {
        Self {
            released: AtomicBool::new(false),
            waiting: Mutex::new(0),
            timeout_ms,
        }
    }

    #[cfg(test)]
    fn wait_for_release(&self, name: &str) {
        *self.waiting.lock().unwrap() += 1;
        let start = std::time::Instant::now();
        while !self.released.load(Ordering::SeqCst) {
            if start.elapsed().as_millis() as u64 > self.timeout_ms {
                panic!(
                    "Checkpoint '{}' timed out after {}ms - likely missing release_all() in test",
                    name, self.timeout_ms
                );
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn release_all(&self) {
        self.released.store(true, Ordering::SeqCst);
        *self.waiting.lock().unwrap() = 0;
    }

    fn reset(&self) {
        self.released.store(false, Ordering::SeqCst);
    }

    fn waiting_count(&self) -> usize {
        *self.waiting.lock().unwrap()
    }
}

/// Capture storage - stores clones of captured values
struct CaptureList<T: Clone> {
    inner: Mutex<Vec<T>>,
}

impl<T: Clone> CaptureList<T> {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn push(&self, value: &T) {
        let mut captures = self.inner.lock().unwrap();
        captures.push(value.clone());
    }

    fn get_all(&self) -> Vec<T> {
        let captures = self.inner.lock().unwrap();
        captures.clone()
    }

    fn clear(&self) {
        let mut captures = self.inner.lock().unwrap();
        captures.clear();
    }
}

/// A checkpoint that threads wait at
pub struct Checkpoint<T: Clone + Send + 'static> {
    name: &'static str,
    blocking: BlockingState,
    captures: CaptureList<T>,
    hook: Mutex<Option<Hook>>,
}

unsafe impl<T: Clone + Send + 'static> Send for Checkpoint<T> {}
unsafe impl<T: Clone + Send + 'static> Sync for Checkpoint<T> {}

impl<T: Clone + Send + 'static + std::fmt::Debug> Checkpoint<T> {
    /// Create a new checkpoint with the given name and a hook for interception
    ///
    /// The hook is called with the debug-formatted value and returns a Decision.
    /// This allows the test to conditionally block/capture based on the value.
    ///
    /// `timeout_ms` is the maximum time a thread will block before panicking.
    /// This prevents tests from hanging forever if release_all() is forgotten.
    pub fn new<F>(name: &'static str, timeout_ms: u64, hook: F) -> Arc<Self>
    where
        F: FnMut(String) -> Decision + Send + 'static,
    {
        Arc::new(Self {
            name,
            blocking: BlockingState::new(timeout_ms),
            captures: CaptureList::new(),
            hook: Mutex::new(Some(Box::new(hook))),
        })
    }

    /// Wait at checkpoint with interception
    ///
    /// Returns `Some(value)` if CaptureAndBlock and released, `None` otherwise.
    /// In production (without cfg(test)), this is a no-op that just returns the value.
    pub fn wait<F: FnOnce() -> T>(&self, provide_value: F) -> Option<T> {
        #[cfg(test)]
        {
            let value = provide_value();

            // Call the hook to get decision
            let decision = {
                let mut hook_guard = self.hook.lock().unwrap();
                if let Some(ref mut h) = *hook_guard {
                    h(format!("{:?}", &value))
                } else {
                    Decision::BlockWithoutCapture
                }
            };

            match decision {
                Decision::CaptureAndBlock => {
                    self.captures.push(&value);
                    self.blocking.wait_for_release(self.name);
                    Some(value)
                }
                Decision::BlockWithoutCapture => {
                    self.blocking.wait_for_release(self.name);
                    None
                }
                Decision::Proceed => None,
            }
        }

        #[cfg(not(test))]
        {
            let _ = &self.name;
            let _ = &self.blocking;
            let _ = &self.captures;
            let _ = &self.hook;
            Some(provide_value())
        }
    }

    /// Release all threads blocked at this checkpoint
    pub fn release_all(&self) {
        self.blocking.release_all();
    }

    /// Reset the checkpoint for reuse
    pub fn reset(&self) {
        self.blocking.reset();
        self.captures.clear();
    }

    /// Get number of waiting threads
    pub fn waiting_count(&self) -> usize {
        self.blocking.waiting_count()
    }

    /// Get all captured values at this checkpoint
    pub fn captures(&self) -> Vec<T> {
        self.captures.get_all()
    }
}

// ========== TESTS ==========

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proceed_decision() {
        let checkpoint = Checkpoint::new("proceed", 5000, |_| Decision::Proceed);
        let result = checkpoint.wait(|| 42i32);
        assert!(result.is_none());
        assert_eq!(checkpoint.waiting_count(), 0);
    }

    #[test]
    fn test_block_without_capture() {
        let checkpoint = Checkpoint::new("block", 5000, |_| Decision::BlockWithoutCapture);
        let started = Arc::new(Mutex::new(false));
        let started_clone = started.clone();

        let checkpoint_clone = checkpoint.clone();
        let handle = thread::spawn(move || {
            *started_clone.lock().unwrap() = true;
            checkpoint_clone.wait(|| 42i32);
            "thread completed"
        });

        // Wait for thread to be at the checkpoint
        for _ in 0..100 {
            if *started.lock().unwrap() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(*started.lock().unwrap());

        assert_eq!(checkpoint.waiting_count(), 1);
        checkpoint.release_all();

        let result = handle.join().unwrap();
        assert_eq!(result, "thread completed");
    }

    #[test]
    fn test_multiple_threads_block() {
        let checkpoint = Checkpoint::new("multi", 5000, |_| Decision::BlockWithoutCapture);
        let counters = Arc::new(Mutex::new(Vec::new()));

        let checkpoint1 = checkpoint.clone();
        let counters1 = counters.clone();
        let t1 = thread::spawn(move || {
            counters1.lock().unwrap().push(1);
            checkpoint1.wait(|| 1i32);
            "t1 done"
        });

        let checkpoint2 = checkpoint.clone();
        let counters2 = counters.clone();
        let t2 = thread::spawn(move || {
            counters2.lock().unwrap().push(2);
            checkpoint2.wait(|| 2i32);
            "t2 done"
        });

        // Wait for both threads to be at the checkpoint
        for _ in 0..100 {
            if counters.lock().unwrap().len() == 2 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(counters.lock().unwrap().len(), 2);
        assert_eq!(checkpoint.waiting_count(), 2);

        checkpoint.release_all();

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert_eq!(r1, "t1 done");
        assert_eq!(r2, "t2 done");
    }

    #[test]
    fn test_capture_and_block() {
        let checkpoint = Checkpoint::new("capture", 5000, |_| Decision::CaptureAndBlock);
        let started = Arc::new(Mutex::new(false));
        let started_clone = started.clone();

        let checkpoint_clone = checkpoint.clone();
        let handle = thread::spawn(move || {
            *started_clone.lock().unwrap() = true;
            checkpoint_clone.wait(|| 100i32);
            "done"
        });

        // Wait for thread to block
        for _ in 0..100 {
            if *started.lock().unwrap() && checkpoint.waiting_count() == 1 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(checkpoint.waiting_count(), 1);

        // Check capture happened
        let caps = checkpoint.captures();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0], 100);

        checkpoint.release_all();

        let result = handle.join().unwrap();
        assert_eq!(result, "done");
    }

    #[test]
    fn test_conditional_capture() {
        // Only capture values > 50
        let checkpoint = Checkpoint::new("conditional", 5000, |v: String| {
            if v.parse::<i32>().map(|n| n > 50).unwrap_or(false) {
                Decision::CaptureAndBlock
            } else {
                Decision::Proceed
            }
        });

        // Value <= 50 should proceed
        let result1 = checkpoint.wait(|| 30i32);
        assert!(result1.is_none());
        assert!(checkpoint.captures().is_empty());

        // Value > 50 should capture and block
        let started = Arc::new(Mutex::new(false));
        let started_clone = started.clone();
        let checkpoint_clone = checkpoint.clone();
        let handle = thread::spawn(move || {
            *started_clone.lock().unwrap() = true;
            checkpoint_clone.wait(|| 100i32)
        });

        for _ in 0..100 {
            if *started.lock().unwrap() && checkpoint.waiting_count() == 1 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        let caps = checkpoint.captures();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0], 100);

        checkpoint.release_all();
        handle.join().unwrap();
    }
}
