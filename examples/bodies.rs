/// This example is mainly intended for `cargo expand --example bodies`, to have an example of
/// macro-expanded code that uses streams and the `=>` syntax.
#[tokio::main]
async fn main() {
    inner().await;
}

async fn inner() {
    join_me_maybe::join!(
        x1 = async { 1 } => x1,
        maybe x2 = async { 2 } => x2,
        label1: x3 = async { 3 } => x3,
        label2: maybe x4 = async { 4 } => x4,
        x5 in futures::stream::iter([5]) => _ = x5,
        maybe x6 in futures::stream::iter([6]) => _ = x6,
        label7: x7 in futures::stream::iter([7]) => _ = x7,
        label8: maybe x8 in futures::stream::iter([8]) => _ = x8,
    );
}
