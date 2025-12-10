//! `join_me_maybe!` is an expanded version of the [`futures::join!`]/[`tokio::join!`] macro, with
//! some added features for cancellation and early exit. Programs that need this sort of thing
//! often resort to "[`select!`] in a loop" and/or "`select!` by reference", but those come with a
//! notoriously long list of footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609] The
//! goal of `join_me_maybe!` is to be more convenient and less error-prone than `select!` in some
//! (not all) of its most common applications. The stretch goal is to make the case that
//! `select!`-by-reference in particular isn't usually necessary and should be _considered
//! harmful_.
//!
//! # Examples
//!
//! The basic use case works like `join!`, polling each of its arguments to completion and
//! returning their outputs in a tuple.
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use join_me_maybe::join_me_maybe;
//! use tokio::time::{sleep, Duration};
//!
//! // Create a couple futures, one that's ready immediately, and another that takes some time.
//! let future1 = std::future::ready(1);
//! let future2 = async { sleep(Duration::from_millis(100)).await; 2 };
//!
//! // Run them concurrently and wait for both of them to finish.
//! let (a, b) = join_me_maybe!(future1, future2);
//! assert_eq!((a, b), (1, 2));
//! # }
//! ```
//!
//! (This is an example to get us started, but in practice I would just use `join!` here.)
//!
//! ## `maybe` cancellation
//!
//! If you don't want to wait for all of your futures finish, you can use the `maybe` keyword. This
//! can be useful with infinite loops of background work that never actually exit. The outputs of
//! `maybe` futures are wrapped in `Option`.
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join_me_maybe;
//! # use tokio::time::{sleep, Duration};
//! # use futures::StreamExt;
//! let outputs = join_me_maybe!(
//!     // This future isn't `maybe`, so we'll definitely wait for it to finish.
//!     async { sleep(Duration::from_millis(100)).await; 1 },
//!     // Same here.
//!     async { sleep(Duration::from_millis(200)).await; 2 },
//!     // We won't necessarily wait for this `maybe` future, but in practice it'll finish before
//!     // the "definitely" futures above, and we'll get its output wrapped in `Some()`.
//!     maybe async { sleep(Duration::from_millis(10)).await; 3 },
//!     // This `maybe` future never finishes. We'll cancel it when the "definitely" work is done.
//!     maybe async {
//!         loop {
//!             // Some periodic work...
//!             sleep(Duration::from_millis(10)).await;
//!         }
//!     },
//! );
//! assert_eq!(outputs, (1, 2, Some(3), None));
//! # }
//! ```
//!
//! ## `label:` and `.cancel()`
//!
//! You can also cancel futures by name if you `label:` them. The outputs of labeled futures are
//! wrapped in `Option` too.
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join_me_maybe;
//! # use tokio::time::{sleep, Duration};
//! # use futures::StreamExt;
//! let mutex = tokio::sync::Mutex::new(42);
//! let outputs = join_me_maybe!(
//!     // The `foo:` label here means that all future expressions (including this one) have a
//!     // `foo` object in scope, which provides a `.cancel()` method.
//!     foo: async {
//!         let mut guard = mutex.lock().await;
//!         *guard += 1;
//!         // Selfishly hold the lock for a long time.
//!         sleep(Duration::from_secs(1_000_000)).await;
//!     },
//!     async {
//!         // Give `foo` a little bit of time...
//!         sleep(Duration::from_millis(100)).await;
//!         if mutex.try_lock().is_err() {
//!             // Hmm, `foo` is taking way too long. Cancel it!
//!             foo.cancel();
//!         }
//!         // Cancelling `foo` drops it promptly, which releases the lock. Note that if it only
//!         // stopped polling `foo`, but didn't drop it, this would be a deadlock. This is a
//!         // common footgun with `select!`-in-a-loop.
//!         *mutex.lock().await
//!     },
//! );
//! assert_eq!(outputs, (None, 43));
//! # }
//! ```
//!
//! A `.cancel()`ed future won't be polled again, and it'll be dropped promptly, freeing any locks
//! or other resources that it might be holding. Note that if a future cancels _itself_, its
//! execution still continues as normal after `.cancel()` returns, up until the next `.await`
//! point. This can be useful in closure bodies or nested `async` blocks, where `return` or `break`
//! doesn't work.
//!
//! ## `no_std`
//!
//! `join_me_maybe!` is compatible with `#![no_std]`. It has no runtime dependencies and does not
//! allocate.
//!
//! [`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
//! [`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
//! [`select!`]: https://tokio.rs/tokio/tutorial/select
//! [cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
//! [rfd400]: https://rfd.shared.oxide.computer/rfd/400
//! [rfd609]: https://rfd.shared.oxide.computer/rfd/609

#![no_std]

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

/// The macro that this crate is all about
///
/// See the [module-level documentation](crate) for details and examples.
pub use join_me_maybe_impl::join_me_maybe;

#[doc(hidden)]
pub mod maybe_done;

/// The type that provides the `.cancel()` method for labeled arguments
pub struct Canceller<'a> {
    finished: &'a AtomicBool,
    definitely_count: Option<&'a AtomicUsize>,
}

impl<'a> Canceller<'a> {
    #[doc(hidden)]
    pub fn new_definitely(finished: &'a AtomicBool, count: &'a AtomicUsize) -> Self {
        Self {
            finished,
            definitely_count: Some(count),
        }
    }

    #[doc(hidden)]
    pub fn new_maybe(finished: &'a AtomicBool) -> Self {
        Self {
            finished,
            definitely_count: None,
        }
    }

    /// Cancel the corresponding labeled future. It won't be polled again, and it will be dropped
    /// promptly, though not directly within this function. Note that if a future cancels _itself_,
    /// its execution continues after `.cancel()` returns until its next `.await` point. It's still
    /// possible for it to return a value. (In an `async` block it's less confusing to just
    /// `return` instead.)
    #[inline]
    pub fn cancel(&self) {
        // No need for atomic compare-exchange or fetch-add here. We're only using atomics to avoid
        // needing to write unsafe code. (Relaxed atomic loads and stores are extremely cheap,
        // often equal to regular ones.)
        if !self.finished.load(Relaxed) {
            self.finished.store(true, Relaxed);
            // The macro calls this method after labeled futures exit naturally, so each definitely
            // future bumps the count exactly once either way.
            if let Some(count) = &self.definitely_count {
                count.store(count.load(Relaxed) + 1, Relaxed);
            }
        }
    }
}
