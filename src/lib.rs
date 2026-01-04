//! `join_me_maybe` provides an expanded version of the [`futures::join!`]/[`tokio::join!`] macro,
//! with added features for cancellation and working with streams. Programs that need this sort of
//! control flow often resort to "[`select!`] in a loop" and/or "`select!` by reference", but those
//! come with a notoriously long list of footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609]
//! The goal of `join_me_maybe` is to be more convenient and less error-prone than `select!` in its
//! most common applications. The stretch goal is to make the case that `select!`-by-reference in
//! particular isn't usually necessary and should be _considered harmful_.
//!
//! # Features and examples
//!
//! The basic use case works like other `join!` macros, polling each of its arguments to completion
//! and returning their outputs in a tuple.
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use join_me_maybe::join;
//! use tokio::time::{sleep, Duration};
//!
//! // Create a couple futures, one that's ready immediately, and another that takes some time.
//! let future1 = std::future::ready(1);
//! let future2 = async { sleep(Duration::from_millis(100)).await; 2 };
//!
//! // Run them concurrently and wait for both of them to finish.
//! let (a, b) = join!(future1, future2);
//! assert_eq!((a, b), (1, 2));
//! # }
//! ```
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
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! # use futures::StreamExt;
//! let outputs = join!(
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
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! # use futures::StreamExt;
//! let mutex = tokio::sync::Mutex::new(42);
//! let outputs = join!(
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
//! One of the powerful features of `select!` is that its arm bodies (though not its "scrutinees")
//! get exclusive mutable access to the enclosing scope. `join_me_maybe!` supports an expanded `=>`
//! syntax that works similarly:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! let mut counter = 0;
//! join!(
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
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! # use futures::FutureExt;
//! let mut counter = 0;
//! join!(
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
//! ## streams
//!
//! Similar to the `=>` syntax for futures above, you can also drive a stream, using `<pattern> in
//! <stream>` instead of `<pattern> = <future>`. In this case the following expression executes for
//! each item in the stream. It gets mutable access to the environment, but it cannot `.await`:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! use futures::stream;
//!
//! let mut total = 0;
//! join!(
//!     n in stream::iter([1, 2, 3]) => total += n,
//!     n in stream::iter([4, 5, 6]) => total += n,
//! );
//! assert_eq!(total, 21);
//! # }
//! ```
//!
//! You can optionally follow this syntax with the `finally` keyword and another expression that
//! executes after the stream is finished (if it's not cancelled). This also gets mutable access to
//! the environment and cannot `.await`. Streams have no return value by default, but streams with
//! a `finally` expression take the value of that expression:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use futures::stream;
//! let ret = join!(
//!     // This stream has no `finally` expression, so it returns `()`.
//!     _ in stream::iter([42]) => {},
//!     // This arm's `finally` expression is `1`, and the `maybe` means we get `Some(1)`.
//!     maybe _ in stream::iter([42]) => {} finally 1,
//!     // Same, but without `maybe` we get the unwrapped value.
//!     _ in stream::iter([42]) => {} finally 2,
//!     // All the streams above finish immediately, so this `maybe` stream gets cancelled and
//!     // returns `None` instead of evaluating its `finally` expression.
//!     maybe _ in stream::iter([42]) => {} finally 3,
//! );
//! assert_eq!(ret, ((), Some(1), 2, None));
//! # }
//! ```
//!
//! Here's an example of driving a stream together with `label:`/`.cancel()`, which works with
//! streams like it does with futures:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! use futures::stream::{self, StreamExt};
//!
//! let mut counter = 0;
//! join!(
//!     my_stream: _ in stream::iter(0..5).then(async |_| {
//!         sleep(Duration::from_millis(10)).await
//!     }) => {
//!         // This stream gets cancelled below, so this only executes three times.
//!         counter += 1;
//!     } finally {
//!         // This stream gets cancelled below, so this will never execute.
//!         counter += 1_000_000;
//!     },
//!     async {
//!         // Wait long enough for the stream to yield three items, then cancel it.
//!         sleep(Duration::from_millis(35)).await;
//!         my_stream.cancel();
//!     },
//! );
//! assert_eq!(counter, 3);
//! # }
//! ```
//!
//! ## mutable access to futures and streams
//!
//! This feature is even more experimental than everything else above. In synchronous expressions
//! with mutable access to the calling scope (those after `=>` and `finally`), `label:` cancellers
//! support an additional method: `.as_pin_mut()`. This returns an `Option<Pin<&mut T>>` pointing
//! to the corresponding future or stream. (Or `None` if it's already completed/cancelled.) You can
//! use this to mutate e.g. a [`FuturesUnordered`] or a [`StreamMap`] to add more work to it while
//! it's being polled. (Not literally while it's being polled, but while it's owned by `join!` and
//! guaranteed not to be "snoozed".) This is intended as an alternative to patterns that await
//! futures *by reference*, which tends to be prone to "snoozing" mistakes.
//!
//! Unfortunately, streams that you can add work to dynamically are usually "poorly behaved" in the
//! sense that they often return `Ready(None)` for a while, until more work is eventually added and
//! they start returning `Ready(Some(_))` again. This is at odds with the [usual rule] that you
//! shouldn't poll a stream again after it returns `Ready(Some)`, but it does work with
//! `select!`-in-a-loop. (In Tokio it requires an `if` guard, and with `futures::select!` it leans
//! on the "fused" requirement.) However, it does _not_ naturally work with `join_me_maybe`, which
//! interprets `Ready(None)` as "end of stream" and promptly drops the whole stream. ([Like it's
//! supposed to!][usual rule]) For a stream to work well with this feature, it needs to do two
//! things that as far as I know none of the dynamic streams currently do:
//!
//! 1. The stream should only ever return `Ready(Some(_))` or `Pending`, until you somehow inform
//!    it that no more work is coming, using say a `.close()` method or something. After that the
//!    stream should probably drain its remaining work before returning `Ready(None)`. (If the
//!    caller doesn't to wait for remaining work, they can cancel the stream instead.)
//! 2. Because adding more work might unblock callers that previously received `Pending`, the
//!    stream should stash a `Waker` and invoke it whenever work is added.
//!
//! Adapting a stream that doesn't behave this way is complicated and not obviously a good idea.
//! [See `tests/test.rs` for some examples.][adapter] Manually tracking `Waker`s is exactly the
//! sort of error-prone business that this crate wants to _discourage_, and this whole feature will
//! need a lot of baking before I can recommend it.
//!
//! ## `no_std`
//!
//! `join_me_maybe` doesn't heap allocate and is compatible with `#![no_std]`.
//!
//! [`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
//! [`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
//! [`select!`]: https://tokio.rs/tokio/tutorial/select
//! [cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
//! [rfd400]: https://rfd.shared.oxide.computer/rfd/400
//! [rfd609]: https://rfd.shared.oxide.computer/rfd/609
//! [`FutureExt::map`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.map
//! [`FutureExt::then`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.then
//! [`AsyncIterator`]: https://doc.rust-lang.org/std/async_iter/trait.AsyncIterator.html
//! [`futures::stream`]: https://docs.rs/futures/latest/futures/stream/
//! [`FuturesUnordered`]: https://docs.rs/futures/latest/futures/stream/struct.FuturesUnordered.html
//! [`StreamMap`]: https://docs.rs/tokio-stream/latest/tokio_stream/struct.StreamMap.html
//! [usual rule]: https://docs.rs/futures/latest/futures/stream/trait.Stream.html#tymethod.poll_next
//! [adapter]: https://github.com/oconnor663/join_me_maybe/blob/672c615cd586140e09052a83795ccc291c0a31c8/tests/test.rs#L180-L327
//!
//! # Help needed! (from the compiler...)
//!
//! As far as I know, there's no way for the `join!` macro to support something like this today:
//!
//! ```rust,compile_fail
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! let mut x = 0;
//! join!(
//!     async { x += 1; },
//!     async { x += 1; }, // error: cannot borrow `x` as mutable more than once at a time
//! );
//! assert_eq!(x, 2);
//! # }
//! ```
//!
//! The problem is that both futures want to capture `&mut x`, which violates the mutable aliasing
//! rule. However, that arguably borrows too much. Consider:
//!
//! 1. Neither of these futures tries to hold `&mut x` across an `.await` point. In other words,
//!    the borrow checker would accept any interleaving of their "basic blocks" in a
//!    single-threaded context.
//! 2. They're hidden inside the join future, so we can't yeet one of them off to another thread to
//!    create a data race.
//!
//! Instead of each inner future capturing `&mut x`, the outer join future could capture it once,
//! and the inner futures could "reborrow" it in some sense when they're polled. My guess is that
//! there's no practical way for a macro to express this in Rust today (corrections welcome!), but
//! the Rust compiler could add a hypothetical syntax like this:
//!
//! ```rust,ignore
//! let mut x = 0;
//! let mut y = 0;
//! concurrent_bikeshed {
//!     {
//!         // `x` is not borrowed across the `.await`...
//!         x += 1;
//!         // ...but `y` is.
//!         let y_ref = &mut y;
//!         sleep(Duration::from_secs(1)).await;
//!         *y_ref += 1;
//!         x += 1;
//!     },
//!     {
//!         // Mutating `x` here does not conflict...
//!         x += 1;
//!         // ...but trying to mutate `y` here would conflict.
//!         // y += 1;
//!     },
//! }
//! ```
//!
//! Another big advantage of adding dedicated syntax for this is that it could support
//! `return`/`break`/`continue` as usual to diverge from inside any arm. That would be especially
//! helpful for error handling with `?`, which is awkward in concurrent contexts today.
//!
//! Aside: All the options for divergence in Rust today would naturally cancel the whole
//! `concurrent_bikeshed`. However, if Rust eventually stabilizes `async gen fn` and the `yield`
//! keyword, then `yield` probably should *not* be allowed inside `concurrent_bikeshed`. Yielding
//! from any arm would snooze the other arms at arbitrary `.await` points (generally not `yield`
//! points), where they could be holding locks. This is deadlock-prone in the same way that pausing
//! or cancelling threads is, and we don't let safe code do either of those things. At the very
//! least it should be a scary warning.

#![no_std]

use atomic_refcell::AtomicRefCell;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

/// The macro that this crate is all about
///
/// See the [module-level documentation](crate) for details and examples.
pub use join_me_maybe_impl::join;

/// The type that provides the `.cancel()` method for labeled arguments
pub struct Canceller<'a, T> {
    finished: &'a AtomicBool,
    definitely_count: Option<&'a AtomicUsize>,
    // `Canceller`s are circular. Even though we don't expose a `Canceller` in-scope to the
    // future/stream that it labels (because that could create cyclic/infinite types and fail to
    // compile), we still have indirect cycles like "arm_1 cancels arm_2, and arm_2 cancels arm_1".
    // You can build a cycle like that using interior mutability, and we do, but it tends to run
    // into trouble when futures have nontrivial `Drop` impls. To work around that, and also to
    // provide our interior mutability, each `Canceller` uses `AtomicPtr` (i.e. a raw pointer) to
    // refer to the labeled arm.
    // SAFETY:
    // 1. The macro owns all the arms and all the `Canceller`s. The caller can't change the order
    //    in which they drop.
    // 2. The `with_pin_mut` method uses unsafe code to re-hydrate shared references, but it goes
    //    through `AtomicRefCell` to turn those into (pinned) mutable references. If a caller tries
    //    to abuse `with_pin_mut` to violate the mutable aliasing rule (convoluted but possible),
    //    we'll just panic instead.
    // 3. Drop safety is the most subtle detail, because dropping a reference cycle raises the
    //    possibility that one member of the cycle could observe another member that's already
    //    dropped. To prevent this, the last object we instantiate and the first object that drops
    //    on the way out, is a guard that sets all the finished flags. The `with_pin_mut` method
    //    checks the finished flag and short-circuits if it's set, so no arm can observe any other
    //    during drop.
    inner_ptr: AtomicPtr<AtomicRefCell<Pin<&'a mut Option<T>>>>,
}

impl<'a, T> Canceller<'a, T> {
    /// Cancel the corresponding labeled future or stream. It won't be polled again, and it'll be
    /// dropped promptly by the `join!` (though not directly within this method).
    pub fn cancel(&self) {
        // It's tempting try to clear the inner `Option` directly (the one inside the
        // `AtomicRefCell`), and that would work in most cases, but it wouldn't work for
        // self-cancellation. The running arm's cell is already mutably borrowed, and re-borrowing
        // it will panic. Just set the finished flag, and trust that the `join!` macro will drop it
        // when it's definitely not being polled.
        let already_cancelled = self.finished.swap(true, Relaxed);
        if !already_cancelled {
            // The macro calls this method if labeled futures exit naturally too, so each
            // definitely future bumps the count exactly once either way.
            if let Some(count) = &self.definitely_count {
                count.fetch_add(1, Relaxed);
            }
        }
    }

    /// Obtain a short-lived `Pin<&mut T>` pointing to the labeled future or stream, for the
    /// duration of the provided closure. If the labeled arm has already finished or been
    /// cancelled, the closure receives `None` instead but still runs.
    ///
    /// Note that if you call this method from the `Drop` impl of your future or stream (you'll
    /// probably never do that, but your evil twin might), the closure will always receive `None`,
    /// regardless of the drop order of the arms. This is necessary to avoid dangling references.
    pub fn with_pin_mut<F, U>(&self, f: F) -> U
    where
        F: FnOnce(Option<Pin<&mut T>>) -> U,
    {
        let mut guard;
        let mut pin_mut = None;
        // SAFETY: Don't even try to re-hydrate the inner reference if `finished` is already set.
        // This isn't really necessary during normal execution (there actually is a window where a
        // future is finished but not yet dropped, but it wouldn't ruin anything to let you see it
        // at that point), however it does matter during drop, because these references are
        // generally cyclic. This relies on the macro to set all the `finished` flags in a drop
        // guard that drops before anything else.
        if !self.finished.load(Relaxed) {
            let inner_ptr = self.inner_ptr.load(Relaxed);
            assert!(!inner_ptr.is_null(), "should be populated");
            let inner_ref = unsafe {
                &*(self.inner_ptr.load(Relaxed) as *const AtomicRefCell<Pin<&mut Option<T>>>)
            };
            // SAFETY: If the caller tries to abuse cycles to violate the mutable aliasing rule,
            // they'll succeed at re-hydrating a shared reference above, but this call to
            // `borrow_mut` will panic without performing any UB.
            guard = inner_ref.borrow_mut();
            pin_mut = guard.as_mut().as_pin_mut();
        }
        f(pin_mut)
    }
}

/// Functions that are only intended for use by the macro
#[doc(hidden)]
pub mod _impl {
    use super::*;
    use core::task::{Context, Poll};
    use futures::{FutureExt, Stream, StreamExt};

    pub fn new_definitely_canceller<'a, T>(
        finished: &'a AtomicBool,
        count: &'a AtomicUsize,
    ) -> Canceller<'a, T> {
        Canceller {
            finished,
            definitely_count: Some(count),
            inner_ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub fn new_maybe_canceller<'a, T>(finished: &'a AtomicBool) -> Canceller<'a, T> {
        Canceller {
            finished,
            definitely_count: None,
            inner_ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    // See the comments about `inner_ptr` above. This function is unsafe in part because it doesn't
    // assert any lifetime bounds on its arguments.
    pub unsafe fn populate<T>(this: &Canceller<'_, T>, inner: &AtomicRefCell<Pin<&mut Option<T>>>) {
        this.inner_ptr.store(inner as *const _ as *mut _, Relaxed);
    }

    // `futures` has `poll!`, but it doesn't have a stream version. The macros is also kind of
    // gross, so just adapt it into a couple wrapper structs
    pub struct PollOnce<Fut: Future + Unpin>(pub Fut);

    impl<Fut: Future + Unpin> Future for PollOnce<Fut> {
        type Output = Poll<Fut::Output>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Ready(self.0.poll_unpin(cx))
        }
    }

    pub struct PollNextOnce<S: Stream + Unpin>(pub S);

    impl<S: Stream + Unpin> Future for PollNextOnce<S> {
        type Output = Poll<Option<S::Item>>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Ready(self.0.poll_next_unpin(cx))
        }
    }

    // See the SAFETY comments above about why this is needed.
    pub struct FinishedFlagDropGuard<'a, Iter>(pub Iter)
    where
        Iter: Iterator<Item = &'a AtomicBool>;

    impl<'a, Iter> Drop for FinishedFlagDropGuard<'a, Iter>
    where
        Iter: Iterator<Item = &'a AtomicBool>,
    {
        fn drop(&mut self) {
            while let Some(finished_flag) = self.0.next() {
                finished_flag.store(true, Relaxed);
            }
        }
    }
}
