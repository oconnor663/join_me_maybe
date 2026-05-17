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
//! ## arm bodies with `=>`
//!
//! One of the powerful features of `select!` is that its arm bodies get exclusive mutable access
//! to the enclosing scope. `join_me_maybe!` supports an expanded `=>` syntax that works similarly:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! let mut counter = 0;
//! let output = join!(
//!     n = std::future::ready(1) => {
//!         counter += n;
//!         42
//!     },
//!     m = async {
//!         sleep(Duration::from_millis(1)).await;
//!         1
//!     } => counter += m, // Mutate the same `counter` in both arms.
//! );
//! assert_eq!(counter, 2);
//! // When a `=>` body is present, the output is the value of the body.
//! assert_eq!(output, (42, ()));
//! # }
//! ```
//!
//! Also like `select!`, it's possible to `.await` or `return` in an arm body. (However, `break` or
//! `continue` in a containing loop are not supported.) Note that a `return` short-circuits the
//! whole containing function, not just the `join!`. This is useful for error handling:
//!
//! ```
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! async fn foo() -> std::io::Result<()> {
//!     let _: ((), bool) = join!(
//!         _ = std::future::ready(1) => {
//!             sleep(Duration::from_millis(1)).await;
//!             return Ok(()); // Return from `foo` (the whole function, not just the `join!`).
//!         },
//!         _ = std::future::ready(2) => {
//!             std::fs::exists("fallible.txt")? // Error handling with `?` also works.
//!         },
//!     );
//!     unreachable!("`return` short-circuits above.");
//! }
//! ```
//!
//! Shared mutation from different arm bodies wouldn't be possible if they ran concurrently.
//! Instead, `join!` only runs one arm body at a time. This is a potential source of surprising
//! timing bugs, and it's best to avoid `.await`ing in arm bodies if you have a choice. However,
//! "scrutinees" futures (the ones to left of the `=>`) do run concurrently, both with each other
//! and with the one running body. Cancelling an arm also cancels its body, if that body is
//! running:
//!
//! ```
//! # use join_me_maybe::join;
//! # use tokio::time::{sleep, Duration};
//! # #[tokio::main]
//! # async fn main() {
//! let mut first_body_started = false;
//! let mut first_body_finished = false;
//! let mut second_body_ran = false;
//! join!(
//!     first: _ = std::future::ready(1) => {
//!         // This body executes first (because the "scrutinee" future `ready(1)` finishes
//!         // immediately), and it tries to sleep ~forever, but `first.cancel()` below ends up
//!         // cancelling it.
//!         first_body_started = true;
//!         sleep(Duration::from_secs(1_000_000)).await;
//!         first_body_finished = true;
//!     },
//!     maybe _ = std::future::ready(2) => {
//!         // This body never runs. Initially it waits for the first body, because only one
//!         // body runs at a time. After the first body is cancelled, there are no more
//!         // "definitely" arms left, so this "maybe" arm is also implicitly cancelled, even
//!         // though by that point the "scrutinee" future `ready(2)` has already finished.
//!         second_body_ran = true;
//!     },
//!     async {
//!         // This is a "scrutinee" future, not an arm body, so it runs concurrently with the
//!         // first body above and successfully cancels it.
//!         sleep(Duration::from_millis(10)).await;
//!         first.cancel();
//!     },
//! );
//! assert!(first_body_started);
//! assert!(!first_body_finished);
//! assert!(!second_body_ran);
//! # }
//! ```
//!
//! ## streams
//!
//! Similar to the `=>` syntax for futures above, you can also drive a stream, using `<pattern> in
//! <stream>` instead of `<pattern> = <future>`. In this case the following expression executes for
//! each item in the stream. As above, bodies get mutable access to the environment and can
//! `.await` or `return`:
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
//!     m in stream::iter([4, 5, 6]) => total += m,
//! );
//! assert_eq!(total, 21);
//! # }
//! ```
//!
//! You can optionally follow this syntax with the `finally` keyword and another expression that
//! executes after the stream is finished (if it's not cancelled). Streams have no return value by
//! default, but streams with a `finally` expression take the value of that expression:
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use join_me_maybe::join;
//! # use futures::stream;
//! # use tokio::time::{sleep, Duration};
//! let ret = join!(
//!     // This stream has no `finally` expression, so it returns `()`.
//!     _ in stream::iter([42]) => {},
//!     // This stream has a `finally` expression.
//!     _ in stream::iter([42]) => {} finally 1,
//!     // This stream has a `finally` block that awaits.
//!     _ in stream::iter([42]) => {} finally {
//!         std::future::ready(2).await
//!     },
//!     // This `maybe` arm has its `finally` expression wrapped in an `Option`.
//!     maybe _ in stream::iter([42]) => {} finally 3,
//!     // Sleep to give the `maybe` arms above time to run.
//!     sleep(Duration::from_millis(10)),
//!     // This `maybe` arm's `finally` block gets cancelled when the sleep above finishes.
//!     maybe _ in stream::iter([42]) => {} finally {
//!         tokio::time::sleep(Duration::from_millis(20)).await;
//!         2
//!     },
//! );
//! assert_eq!(ret, ((), 1, 2, Some(3), (), None));
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
//!         sleep(Duration::from_millis(20)).await
//!     }) => {
//!         // This stream gets cancelled below, so this only executes three times.
//!         counter += 1;
//!     } finally {
//!         // This stream gets cancelled below, so this will never execute.
//!         counter += 1_000_000;
//!     },
//!     async {
//!         // Wait long enough for the stream to yield three items, then cancel it.
//!         sleep(Duration::from_millis(70)).await;
//!         my_stream.cancel();
//!     },
//! );
//! assert_eq!(counter, 3);
//! # }
//! ```
//!
//! ## mutable access to futures and streams
//!
//! This feature is even more experimental than everything else above. In arm bodies
//! (blocks/expressions after `=>` and `finally`), `label:` cancellers support a couple additional
//! methods: `with_pin_mut` and (for `Unpin` types) `with_mut`. These take a closure and invoke it
//! with an `Option<Pin<&mut T>>` (`Option<&mut T>` respectively) pointing to the corresponding
//! future or stream, or `None` if that arm is already completed or cancelled. You can use this to
//! mutate e.g. a [`FuturesUnordered`] or a [`StreamMap`] to add more work to it while it's being
//! polled. (Not literally while it's being polled -- everything in a `join!` runs on one thread --
//! but while it's owned by `join!` and guaranteed not to be "snoozed".) This is intended as an
//! alternative to patterns that await futures *by reference*, which tends to be prone to
//! "snoozing" mistakes.
//!
//! Unfortunately, streams that you can add input to dynamically are often "poorly behaved" in that
//! that they can return `Ready(None)` for a while, until more work is added and they start
//! returning `Ready(Some(_))` again. This is at odds with the [usual rule] that you shouldn't poll
//! a stream again after it returns `Ready(None)`, but it does work with `select!`-in-a-loop. (In
//! Tokio it requires an `if` guard, and with `futures::select!` it leans on the "fused"
//! requirement.) However, it does _not_ naturally work with `join_me_maybe`, which interprets
//! `Ready(None)` as "end of stream" and promptly drops the whole stream. ([Like it's supposed
//! to!][usual rule]) For a stream to work well with this feature, it needs to behave like a
//! channel:
//!
//! 1. Polling the stream should only ever return `Ready(Some(_))` or `Pending`, until you tell it
//!    that no more input is coming, for example by dropping some sort of writer or calling some
//!    sort of close method. After that the stream should drain its remaining output before
//!    returning `Ready(None)`. (Callers who don't care about draining output can cancel the
//!    stream.)
//!
//! 2. Because adding input can unblock callers who previously received `Pending`, the stream should
//!    stash a `Waker` and generally invoke it when input is added.
//!
//! Adapting a stream that doesn't behave this way is complicated and not obviously a good idea.
//! [See `tests/test.rs` for some examples.][adapter] Manually tracking `Waker`s is exactly the
//! sort of error-prone business that this crate wants to _discourage_, and this whole feature will
//! need a lot of baking before I can recommend it. However, this approach is necessary to solve
//! cases like Niko Matsakis' [case study of pub-sub in mini-redis][miniredis].
//!
//! [miniredis]: https://smallcultfollowing.com/babysteps/blog/2022/06/13/async-cancellation-a-case-study-of-pub-sub-in-mini-redis/
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
//! As far as I know, there's no way for the `join!` macro to support something like this today
//! (this works in arm bodies, but not as here in the "scrutinee" position):
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
//! helpful for error handling with `?`, which is awkward in a lot of concurrent contexts today.

#![no_std]

use atomic_refcell::AtomicRefCell;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

/// The macro that this crate is all about
///
/// See the [module-level documentation](crate) for details and examples.
pub use join_me_maybe_impl::join;

/// The type that provides the `.cancel()` method for labeled arguments
pub struct Canceller<'a> {
    cancelled: &'a AtomicBool,
    finished: Option<&'a AtomicBool>, // only Some for "definitely" arms
    definitely_count: &'a AtomicUsize,
}

impl<'a> Canceller<'a> {
    /// Cancel the corresponding labeled future or stream. It won't be polled again, and it'll be
    /// dropped promptly by the `join!` (though not directly within this method).
    pub fn cancel(&self) {
        // We can't drop the corresponding future/stream here, even if we had a reference to it,
        // because in the self-cancellation case the AtomicRefCell would panic.
        self.cancelled.store(true, Relaxed);
        if let Some(finished) = self.finished {
            let already_finished = finished.swap(true, Relaxed);
            if !already_finished {
                self.definitely_count.fetch_add(1, Relaxed);
            }
        }
    }
}

impl<'a> core::fmt::Debug for Canceller<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Canceller").finish_non_exhaustive()
    }
}

/// The canceller type visible in arm bodies, which supports [`with_pin_mut`][Self::with_pin_mut]
/// and [`with_mut`][Self::with_mut].
pub struct CancellerMut<'a, T> {
    canceller: Canceller<'a>,
    labeled_cell: &'a AtomicRefCell<Pin<&'a mut Option<T>>>,
}

impl<'a, T> CancellerMut<'a, T> {
    /// Cancel the corresponding labeled future or stream. It won't be polled again, and it'll be
    /// dropped promptly by the `join!` (though not directly within this method).
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    /// Obtain a short-lived `Pin<&mut T>` pointing to the labeled future or stream, for the
    /// duration of the provided closure. If the labeled arm has already finished or been
    /// cancelled, the closure receives `None` instead but still runs.
    ///
    /// Internally, each labeled arm is owned by an [`AtomicRefCell`]. If you nest calls to
    /// `with_pin_mut` and try to borrow the same arm twice, the second call will panic.
    ///
    /// [`AtomicRefCell`]: https://docs.rs/atomic_refcell/latest/atomic_refcell/struct.AtomicRefCell.html
    pub fn with_pin_mut<F, U>(&self, f: F) -> U
    where
        F: FnOnce(Option<Pin<&mut T>>) -> U,
    {
        f(self.labeled_cell.borrow_mut().as_mut().as_pin_mut())
    }

    /// Like [`with_pin_mut`][Self::with_pin_mut] above but without `Pin`. This requires the
    /// underlying type to be `Unpin`.
    pub fn with_mut<F, U>(&self, f: F) -> U
    where
        F: FnOnce(Option<&mut T>) -> U,
        T: Unpin,
    {
        f(self.labeled_cell.borrow_mut().as_mut().get_mut().as_mut())
    }
}

// SAFETY: CancellerMut is Send+Sync whenever T is Send, for the same reason as std::sync::Mutex.
// It only hands out mutable references to its contents. (I.e. there is no `with_pin_ref` method.)
// If the contents are !Sync, those references won't be able to escape the thread they wind up on
// for as long as they're alive.
unsafe impl<'a, T: Send> Send for CancellerMut<'a, T> {}
unsafe impl<'a, T: Send> Sync for CancellerMut<'a, T> {}

impl<'a, T> core::fmt::Debug for CancellerMut<'a, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CancellerMut").finish_non_exhaustive()
    }
}

/// Functions that are only intended for use by the macro
#[doc(hidden)]
pub mod _impl {
    use super::*;
    use core::task::{Context, Poll};
    use futures::{FutureExt, Stream, StreamExt};

    pub fn new_canceller<'a>(
        cancelled: &'a AtomicBool,
        finished: Option<&'a AtomicBool>,
        definitely_count: &'a AtomicUsize,
    ) -> Canceller<'a> {
        Canceller {
            cancelled,
            finished,
            definitely_count,
        }
    }

    pub fn new_canceller_mut<'a, T>(
        cancelled: &'a AtomicBool,
        finished: Option<&'a AtomicBool>,
        definitely_count: &'a AtomicUsize,
        labeled_cell: &'a AtomicRefCell<Pin<&'a mut Option<T>>>,
    ) -> CancellerMut<'a, T> {
        CancellerMut {
            canceller: Canceller {
                cancelled,
                finished,
                definitely_count,
            },
            labeled_cell,
        }
    }

    // `futures` has `poll!`, but it doesn't have a stream version. The macro is also kind of
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

    // The macro needs `AtomicRefCell` internally.
    pub use atomic_refcell::AtomicRefCell;

    // This is the only thing from `futures` that the macro needs internally. It's also annoying to
    // deal with post-`cargo expand`. Just wrap it.
    pub async fn yield_once() {
        futures::pending!();
    }
}
