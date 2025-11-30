# `join_me_maybe!` [![crates.io](https://img.shields.io/crates/v/join_me_maybe.svg)](https://crates.io/crates/join_me_maybe) [![docs.rs](https://docs.rs/join_me_maybe/badge.svg)](https://docs.rs/join_me_maybe)

`join_me_maybe!` is an expanded version of the [`futures::join!`]/[`tokio::join!`] macro, with
some added features for cancellation and early exit. Programs that need this sort of thing
often resort to "[`select!`] in a `loop`", but that comes with a notoriously long list of
footguns.[\[1\]][cancelling_async][\[2\]][rfd400][\[3\]][rfd609] The goal of `join_me_maybe!`
is to be more convenient and less error-prone than `select!`-in-a-`loop` for its most common
applications. The stretch goal is to make the case that `select!`-in-a-`loop` should be
_considered harmful_, as they say.

## Examples

The basic use case works just like `join!`, polling each of its arguments to completion and
returning their outputs in a tuple:

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

(That's an example to get us started, but of course if `join!` is all you need, just use
`join!`.)

### `maybe` cancellation

If you don't necessarily want to wait for all of your futures finish, you can use the `maybe`
keyword. For example, this can be useful with infinite loops of background work that never
actually exit. The outputs of `maybe` futures are wrapped in `Option`:

```rust
let outputs = join_me_maybe!(
    // This future isn't `maybe`, so we'll definitely wait for it to finish.
    async { sleep(Duration::from_millis(100)).await; 1 },
    // We won't necessarily wait for this `maybe` future, but in practice it'll finish before
    // the "definitely" future above, and we'll get its output.
    maybe async { sleep(Duration::from_millis(10)).await; 2 },
    // This `maybe` future never finishes, and we'll cancel it when the first future is done.
    maybe async {
        loop {
            // Some periodic work...
            sleep(Duration::from_millis(10)).await;
        }
    },
);
assert_eq!(outputs, (1, Some(2), None));
```

### `label:` and `.cancel()`

It's also possible to cancel futures by name if you `label:` them. The outputs of these are
also wrapped in `Option`:

```rust
let mutex = tokio::sync::Mutex::new(42);
let outputs = join_me_maybe!(
    // The `foo:` label here means all the future expressions here (including this one) have a
    // `foo` object in scope, which provides a `.cancel()` method.
    foo: async {
        let _guard = mutex.lock().await;
        // Selfishly hold the lock for a long time.
        sleep(Duration::from_secs(1_000_000)).await;
    },
    async {
        // Give `foo` a little bit of time...
        sleep(Duration::from_millis(100)).await;
        assert!(mutex.try_lock().is_err(), "foo still has the lock");
        // Hmm, `foo` is taking way too long. Cancel it!
        foo.cancel();
        // Cancelling `foo` drops it promptly, so taking the lock won't block now.
        *mutex.lock().await
    },
);
assert_eq!(outputs, (None, 42));
```

Note that if a future cancels _itself_ in this way, its execution still continues as normal
after `.cancel()` returns, up until the next `.await` point. (In an `async` block, it's usually
cleaner to just `break` or `return`.) In any case, a `.cancel()`ed future won't be polled
again, and it'll be dropped promptly, freeing any locks or other resources that it might be
holding.

### `no_std`

`join_me_maybe!` works under `#![no_std]`. It has no runtime dependencies and does not
allocate.

[`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
[`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
[`select!`]: https://tokio.rs/tokio/tutorial/select
[cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
[rfd400]: https://rfd.shared.oxide.computer/rfd/400
[rfd609]: https://rfd.shared.oxide.computer/rfd/609
