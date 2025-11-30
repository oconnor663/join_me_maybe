# `join_me_maybe!` [![crates.io](https://img.shields.io/crates/v/join_me_maybe.svg)](https://crates.io/crates/join_me_maybe) [![docs.rs](https://docs.rs/join_me_maybe/badge.svg)](https://docs.rs/join_me_maybe)

`join_me_maybe!` is an expanded version of the [`futures::join!`]/[`tokio::join!`] macro, with
some added features for cancellation and early exit. Programs that need this sort of thing
often resort to "[`select!`] in a `loop`", but that comes with a notoriously long list of
footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609] The goal of `join_me_maybe!`
is to be more convenient and less error-prone than `select!`-in-a-`loop` for its most common
applications. The stretch goal is to make the case that `select!`-in-a-`loop` should be
_considered harmful_, as they say.

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

### `no_std`

`join_me_maybe!` is compatible with `#![no_std]`. It has no runtime dependencies and does not
allocate.

## What's wrong with `select!`-in-a-`loop`?

Here are several different mistakes you can make with `select!`-in-a-`loop` that you don't have
to worry about with `join_me_maybe!`.

### Excessive cancellation

In this example the author _intended_ to call `background_work` every second while processing
messages from a channel. However, when messages are coming more than once a second,
`background_work` never gets called at all, because the `sleep` future unintentionally gets
cancelled and recreated every time through the loop:

```rust
loop {
    select! {
        message = receiver.recv() => {
            if let Some(value) = message {
                do_something(value);
            } else {
                break;
            }
        },
        _ = sleep(Duration::from_secs(1)) => {
            background_work();  // This might never run!
        }
    }
}
```

Compare:

```rust
join_me_maybe!(
    async {
        while let Some(value) = receiver.recv().await {
            do_something(value);
        }
    },
    maybe async {
        loop {
            sleep(Duration::from_secs(1)).await;
            background_work();
        }
    },
);
```

### Serial arm bodies

In this example the author _intended_ to process messages from two channels concurrently.
However, whenever the first arm receives a message and sleeps, the second arm also sleeps,
because the bodies of `select!` arms don't run concurrently:

```rust
loop {
    select! {
        Some(_) = receiver1.recv() => {
            println!("1");
            // Some expensive processing...
            sleep(Duration::from_secs(1)).await;
        },
        Some(_) = receiver2.recv() => {
            println!("2");  // The sleep above blocks this!
        },
        else => {
            break;
        }
    }
}
```

Compare (`join!` is sufficient here):

```rust
join!(
    async {
        while let Some(_) = receiver1.recv().await {
            println!("1");
            // Some expensive processing...
            sleep(Duration::from_secs(1)).await;
        }
    },
    async {
        while let Some(_) = receiver2.recv().await {
            println!("2");
        }
    },
);
```

### Delayed `drop`

To fix the excessive cancellation issue above, we often create futures outside the loop and
`select!` on them by mutable reference. That creates a disconnect between when a future stops
being polled and when it gets _dropped_, which can matter for releasing resources like lock
guards.

```rust
let mutex = tokio::sync::Mutex::new(42);
let mut slow_future = pin!(async {
    let _guard = mutex.lock().await;
    // Very slow! This is gonna get cancelled...
    sleep(Duration::from_secs(1_000_000)).await;
});
loop {
    select! {
        _ = &mut slow_future => {}
        _ = sleep(Duration::from_millis(100)) => {
            if should_cancel() {
                break;
            }
        },
    }
}
// `slow_future` is no longer being polled, but it hasn't been dropped.
let _guard = mutex.lock().await; // Deadlock!
```

Compare:

```rust
let mutex = tokio::sync::Mutex::new(42);
join_me_maybe!(
    slow_future: async {
        let _guard = mutex.lock().await;
        // Very slow! This is gonna get cancelled...
        sleep(Duration::from_secs(1_000_000)).await;
    },
    async {
        loop {
            sleep(Duration::from_millis(100)).await;
            if should_cancel() {
                slow_future.cancel();
                // `slow_future` gets dropped promptly. No risk of deadlock.
                let _guard = mutex.lock().await;
                break;
            }
        }
    }
);
```

[`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
[`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
[`select!`]: https://tokio.rs/tokio/tutorial/select
[cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
[rfd400]: https://rfd.shared.oxide.computer/rfd/400
[rfd609]: https://rfd.shared.oxide.computer/rfd/609
