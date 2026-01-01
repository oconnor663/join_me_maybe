/// This example is mainly intended for `cargo expand --example finally`, to have an example of
/// macro-expanded code that uses streams and the `finally` keyword.
#[tokio::main]
async fn main() {
    inner().await;
}

async fn inner() {
    join_me_maybe::join!(
        x1 in futures::stream::iter([1]) => _ = x1 finally 1,
        maybe x2 in futures::stream::iter([2]) => _ = x2 finally 2,
        label3: x3 in futures::stream::iter([3]) => _ = x3 finally 3,
        label4: maybe x4 in futures::stream::iter([4]) => _ = x4 finally 4,
    );
}
