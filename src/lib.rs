//! `join_me_maybe!` is an expanded version of the [`futures::join!`]/[`tokio::join!`] macro, with
//! several added features for cancellation, early exit, and mutable access to the enclosing scope.
//! Programs that need this sort of control flow often resort to "[`select!`] in a loop" and/or
//! "`select!` by reference", but those come with a notoriously long list of
//! footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609] The goal of `join_me_maybe!`
//! is to be more convenient and less error-prone than `select!` in its most common applications.
//! The stretch goal is to make the case that `select!`-by-reference in particular isn't usually
//! necessary and should be _considered harmful_.
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
//! ## "finish expressions": `<pattern> = <future> => <body>`
//!
//! One of the most powerful properties of `select!` is that its arms get exclusive mutable access
//! to the enclosing scope when they run. `join_me_maybe!` futures run concurrently and can't
//! mutate shared variables (without a `Mutex` or some other source of "interior mutability"). To
//! close this cap, `join_me_maybe!` supports an expanded, select-like syntax:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join_me_maybe;
//! # use tokio::time::{sleep, Duration};
//! let mut counter = 0;
//! join_me_maybe!(
//!     _ = sleep(Duration::from_millis(1)) => counter += 1,
//!     n = async {
//!         sleep(Duration::from_millis(1)).await;
//!         1
//!     } => counter += n,
//! );
//! assert_eq!(counter, 2);
//! # }
//! ```
//!
//! In order to give these "finish expressions" mutable access to the enclosing scope, without
//! "snoozing" any of the other concurrent futures, these expressions run in a _synchronous
//! context_ (i.e. they cannot `.await`). This makes them similar to [`FutureExt::map`] (as opposed
//! to [`FutureExt::then`]). However, note that trying to accomplish the same thing with `map` (or
//! `then`) doesn't compile:
//!
//! ```compile_fail
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join_me_maybe;
//! # use tokio::time::{sleep, Duration};
//! # use futures::FutureExt;
//! let mut counter = 0;
//! join_me_maybe!(
//!     sleep(Duration::from_millis(1)).map(|_| counter += 1),
//!     //                                      ------- first mutable borrow
//!     async {
//!         sleep(Duration::from_millis(1)).await;
//!         1
//!     }.map(|n| counter += n),
//!     //        ------- second mutable borrow
//! );
//! # }
//! ```
//!
//! ## `no_std`
//!
//! `join_me_maybe!` doesn't heap allocate and is compatible with `#![no_std]`.
//!
//! [`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
//! [`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
//! [`select!`]: https://tokio.rs/tokio/tutorial/select
//! [cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
//! [rfd400]: https://rfd.shared.oxide.computer/rfd/400
//! [rfd609]: https://rfd.shared.oxide.computer/rfd/609
//! [`FutureExt::map`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.map
//! [`FutureExt::then`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.then

#![no_std]

use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

/// The macro that this crate is all about
///
/// See the [module-level documentation](crate) for details and examples.
pub use join_me_maybe_impl::join_me_maybe;

/// The type that provides the `.cancel()` method for labeled arguments
pub struct Canceller<'a> {
    finished: &'a AtomicBool,
    definitely_count: Option<&'a AtomicUsize>,
}

impl<'a> Canceller<'a> {
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

/// Like `Canceller`, but in scope for `=>` expressions/blocks with mutable access the environment
///
/// In addition to the `.cancel()` method, this type provides the `.inner()` for accessing the
/// underlying future or stream. This feature is highly experimental. The main intended use case is
/// streams like [`FuturesUnordered`], which let you add more work dynamically. This makes it
/// possible for one arm of `join_me_maybe!` to add more work to another arm.
///
/// However, note that `join_me_maybe!` drops futures and streams after they return
/// `Ready`/`Ready(None)` (or when they're cancelled), while `FuturesUnordered` often return
/// `Ready(None)` temporarily until it's later refilled. In my opinion, this makes
/// `FuturesUnordered` a "poorly behaved" stream. In practice it's almost always polled with
/// `select!`-in-a-loop, which has the interesting property of re-polling all arms whenever any arm
/// returns non-`Pending`. (Maybe more intuitively, we'd say that it returns `Pending` when all
/// arms are `Pending`.) On the other hand, `join_me_maybe!` (currently) effectively re-polls all
/// arms whenever any arm *requests a wakeup*, so you really need adding work itself to trigger a
/// wakeup. It might be that the designers didn't imagine it was *possible* to call
/// `FuturesUnordered::push` while the whole thing was effectively being awaited? This feature
/// makes it possible, which is interesting but tricky.
///
/// [`FuturesUnordered`]: https://docs.rs/futures/latest/futures/stream/struct.FuturesUnordered.html
pub struct CancellerMut<'a, 'b, T> {
    canceller: &'a Canceller<'a>,
    inner: Option<Pin<&'b mut T>>,
}

impl<'a, 'b, T> CancellerMut<'a, 'b, T> {
    #[inline]
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    #[inline]
    pub fn inner(&mut self) -> Option<Pin<&mut T>> {
        self.inner.as_mut().map(|p| p.as_mut())
    }
}

/// Functions that are only intended for use by the macro
#[doc(hidden)]
pub mod _impl {
    use super::*;
    use core::task::{Context, Poll};
    use futures::Stream;
    use pin_project_lite::pin_project;

    #[inline]
    pub fn new_definitely_canceller<'a>(
        finished: &'a AtomicBool,
        count: &'a AtomicUsize,
    ) -> Canceller<'a> {
        Canceller {
            finished,
            definitely_count: Some(count),
        }
    }

    #[inline]
    pub fn new_maybe_canceller<'a>(finished: &'a AtomicBool) -> Canceller<'a> {
        Canceller {
            finished,
            definitely_count: None,
        }
    }

    #[inline]
    pub fn new_canceller_mut<'a, 'b, T>(
        canceller: &'a Canceller,
        inner: Option<Pin<&'b mut T>>,
    ) -> CancellerMut<'a, 'b, T> {
        CancellerMut { canceller, inner }
    }

    pub fn fuse_future<Fut>(fut: Fut) -> FuseFuture<Fut> {
        FuseFuture { inner: Some(fut) }
    }

    // We can't use `futures::future::Fuse`, because it doesn't expose the inner future.
    pin_project! {
        pub struct FuseFuture<Fut> {
            #[pin]
            inner: Option<Fut>,
        }
    }

    impl<Fut> FuseFuture<Fut> {
        pub fn is_done(&self) -> bool {
            self.inner.is_none()
        }

        pub fn get_pin_mut(self: Pin<&mut Self>) -> Option<Pin<&mut Fut>> {
            self.project().inner.as_pin_mut()
        }

        pub fn cancel(self: Pin<&mut Self>) {
            self.project().inner.set(None);
        }
    }

    impl<Fut: Future> Future for FuseFuture<Fut> {
        type Output = Fut::Output;

        #[inline]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Fut::Output> {
            match self.as_mut().project().inner.as_pin_mut() {
                Some(fut) => fut.poll(cx).map(|output| {
                    self.project().inner.set(None);
                    output
                }),
                None => Poll::Pending,
            }
        }
    }

    pub fn fuse_stream<S>(stream: S) -> FuseStream<S> {
        FuseStream {
            inner: Some(stream),
        }
    }

    // We could use `futures::stream::Stream`, because it does expose the inner stream, but it
    // doesn't drop the inner stream the way the future version does.
    pin_project! {
        pub struct FuseStream<S> {
            #[pin]
            inner: Option<S>,
        }
    }

    impl<S> FuseStream<S> {
        pub fn is_done(&self) -> bool {
            self.inner.is_none()
        }

        pub fn get_pin_mut(self: Pin<&mut Self>) -> Option<Pin<&mut S>> {
            self.project().inner.as_pin_mut()
        }

        pub fn cancel(self: Pin<&mut Self>) {
            self.project().inner.set(None);
        }
    }

    impl<S: Stream> Stream for FuseStream<S> {
        type Item = S::Item;

        #[inline]
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<S::Item>> {
            match self.as_mut().project().inner.as_pin_mut() {
                Some(stream) => match stream.poll_next(cx) {
                    Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
                    Poll::Ready(None) => {
                        self.project().inner.set(None);
                        Poll::Ready(None)
                    }
                    Poll::Pending => Poll::Pending,
                },
                None => Poll::Ready(None),
            }
        }
    }
}
