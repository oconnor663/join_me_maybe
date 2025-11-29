#![no_std]

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

pub use join_me_maybe_impl::join_me_maybe;

#[doc(hidden)]
pub mod maybe_done;

/// The type that provides the `.cancel()` method for labeled arms
pub struct Canceller<'a> {
    finished: &'a AtomicBool,
    count: Option<&'a AtomicUsize>,
}

impl<'a> Canceller<'a> {
    #[doc(hidden)]
    pub fn new_definitely(finished: &'a AtomicBool, count: &'a AtomicUsize) -> Self {
        Self {
            finished,
            count: Some(count),
        }
    }

    #[doc(hidden)]
    pub fn new_maybe(finished: &'a AtomicBool) -> Self {
        Self {
            finished,
            count: None,
        }
    }

    /// Cancel the corresponding arm. It won't be polled again, and it will be dropped promptly,
    /// though not immediately within this function. Note that if an arm cancels _itself_, that
    /// doesn't automatically force it to yield, and it could still return a value if it does so
    /// without hitting an `.await`.
    pub fn cancel(&self) {
        // No need for atomic compare-exchange or fetch-add here. We're only using atomics to avoid
        // needing to write unsafe code. (Relaxed atomic loads and stores are extremely cheap,
        // often equal to regular ones.)
        if !self.finished.load(Relaxed) {
            self.finished.store(true, Relaxed);
            // To keep this count accurate, we rely on each arm to mark itself finished if it exits
            // naturally. This is done in the macro.
            if let Some(count) = &self.count {
                count.store(count.load(Relaxed) + 1, Relaxed);
            }
        }
    }
}
