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
let future2 = async {
    sleep(Duration::from_millis(10)).await;
    2
};

// Wait for both of them to finish.
let (a, b) = join_me_maybe!(future1, future2);
assert_eq!((a, b), (1, 2));
```

If you have a a future that you want to run concurrently, but you don't necessarily want to
wait for it to finish, you can use the `maybe` keyword. This can be useful with timer loops
that never actually finish. The outputs of `maybe` futures are wrapped in `Option`:

```rust
let outputs = join_me_maybe!(
    // This future isn't `maybe`, so we'll wait for it to finish.
    async {
        sleep(Duration::from_millis(100)).await;
        1
    },

    // We won't necessarily wait for this `maybe` future, but in practice it'll finish before
    // the "definitely" futures above, and we'll get its output.
    maybe std::future::ready(2),

    // This `maybe` future never finishes, and we'll cancel it when the first future is done.
    maybe async {
        loop {
            sleep(Duration::from_millis(10)).await;
            // some background work
        }
    },
);
assert_eq!(outputs, (1, Some(2), None));
```


[`futures::join!`]: https://docs.rs/futures/latest/futures/macro.join.html
[`tokio::join!`]: https://docs.rs/tokio/latest/tokio/macro.join.html
[`select!`]: https://tokio.rs/tokio/tutorial/select
[cancelling_async]: https://sunshowers.io/posts/cancelling-async-rust/
[rfd400]: https://rfd.shared.oxide.computer/rfd/400
[rfd609]: https://rfd.shared.oxide.computer/rfd/609
