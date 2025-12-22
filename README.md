# `join_me_maybe!` [![crates.io](https://img.shields.io/crates/v/join_me_maybe.svg)](https://crates.io/crates/join_me_maybe) [![docs.rs](https://docs.rs/join_me_maybe/badge.svg)](https://docs.rs/join_me_maybe)

`join_me_maybe!` is an expanded version of the [`futures::join!`]/[`tokio::join!`] macro, with
several added features for cancellation, early exit, and mutable access to the enclosing scope.
Programs that need this sort of control flow often resort to "[`select!`] in a loop" and/or
"`select!` by reference", but those come with a notoriously long list of
footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609] The goal of `join_me_maybe!`
is to be more convenient and less error-prone than `select!` in its most common applications.
The stretch goal is to make the case that `select!`-by-reference in particular isn't usually
necessary and should be _considered harmful_.

## Examples

The basic use case works like `join!`, polling each of its arguments to completion and
returning their outputs in a tuple.

```rust
use join_me_maybe::join_me_maybe;
use tokio::time::{sleep, Duration};

// Create a couple futures, one that's ready immediately, and another that takes some time.
let future1 = std::future::ready(1);
let future2 = async { sleep(Duration::from_millis(100)).await; 2 };

// Run them concurrently and wait for both of them to finish.
let (a, b) = join_me_maybe!(future1, future2);
assert_eq!((a, b), (1, 2));
```

(This is an example to get us started, but in practice I would just use `join!` here.)

### `maybe` cancellation

If you don't want to wait for all of your futures finish, you can use the `maybe` keyword. This
can be useful with infinite loops of background work that never actually exit. The outputs of
`maybe` futures are wrapped in `Option`.

```rust
let outputs = join_me_maybe!(
    // This future isn't `maybe`, so we'll definitely wait for it to finish.
    async { sleep(Duration::from_millis(100)).await; 1 },
    // Same here.
    async { sleep(Duration::from_millis(200)).await; 2 },
    // We won't necessarily wait for this `maybe` future, but in practice it'll finish before
    // the "definitely" futures above, and we'll get its output wrapped in `Some()`.
    maybe async { sleep(Duration::from_millis(10)).await; 3 },
    // This `maybe` future never finishes. We'll cancel it when the "definitely" work is done.
    maybe async {
        loop {
            // Some periodic work...
            sleep(Duration::from_millis(10)).await;
        }
    },
);
assert_eq!(outputs, (1, 2, Some(3), None));
```

### `label:` and `.cancel()`

You can also cancel futures by name if you `label:` them. The outputs of labeled futures are
wrapped in `Option` too.

```rust
let mutex = tokio::sync::Mutex::new(42);
let outputs = join_me_maybe!(
    // The `foo:` label here means that all future expressions (including this one) have a
    // `foo` object in scope, which provides a `.cancel()` method.
    foo: async {
        let mut guard = mutex.lock().await;
        *guard += 1;
        // Selfishly hold the lock for a long time.
        sleep(Duration::from_secs(1_000_000)).await;
    },
    async {
        // Give `foo` a little bit of time...
        sleep(Duration::from_millis(100)).await;
        if mutex.try_lock().is_err() {
            // Hmm, `foo` is taking way too long. Cancel it!
            foo.cancel();
        }
        // Cancelling `foo` drops it promptly, which releases the lock. Note that if it only
        // stopped polling `foo`, but didn't drop it, this would be a deadlock. This is a
        // common footgun with `select!`-in-a-loop.
        *mutex.lock().await
    },
);
assert_eq!(outputs, (None, 43));
```

A `.cancel()`ed future won't be polled again, and it'll be dropped promptly, freeing any locks
or other resources that it might be holding. Note that if a future cancels _itself_, its
execution still continues as normal after `.cancel()` returns, up until the next `.await`
point. This can be useful in closure bodies or nested `async` blocks, where `return` or `break`
doesn't work.

### "finish expressions": `<pattern> = <future> => <body>`

One of the most powerful properties of `select!` is that its arms get exclusive mutable access
to the enclosing scope when they run. `join_me_maybe!` futures run concurrently and can't
mutate shared variables (without a `Mutex` or some other source of "interior mutability"). To
close this cap, `join_me_maybe!` supports an expanded, select-like syntax:

```rust
let mut counter = 0;
join_me_maybe!(
    _ = sleep(Duration::from_millis(1)) => counter += 1,
    n = async {
        sleep(Duration::from_millis(1)).await;
        1
    } => counter += n,
);
assert_eq!(counter, 2);
```

In order to give these "finish expressions" mutable access to the enclosing scope, without
"snoozing" any of the other concurrent futures, these expressions run in a _synchronous
context_ (i.e. they cannot `.await`). This makes them similar to [`FutureExt::map`] (as opposed
to [`FutureExt::then`]). However, note that trying to accomplish the same thing with `map` (or
`then`) doesn't compile:

```compile_fail
# #[tokio::main]
# async fn main() {
# use join_me_maybe::join_me_maybe;
# use tokio::time::{sleep, Duration};
# use futures::FutureExt;
let mut counter = 0;
join_me_maybe!(
    sleep(Duration::from_millis(1)).map(|_| counter += 1),
    //                                      ------- first mutable borrow
    async {
        sleep(Duration::from_millis(1)).await;
        1
    }.map(|n| counter += n),
    //        ------- second mutable borrow
);
# }
```

### `no_std`

`join_me_maybe!` doesn't heap allocate and is compatible with `#![no_std]`.

[`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
[`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
[`select!`]: https://tokio.rs/tokio/tutorial/select
[cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
[rfd400]: https://rfd.shared.oxide.computer/rfd/400
[rfd609]: https://rfd.shared.oxide.computer/rfd/609
[`FutureExt::map`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.map
[`FutureExt::then`]: https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.then
