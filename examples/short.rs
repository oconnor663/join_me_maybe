/// This example is mainly intended for `cargo expand --example short`, to have an example of
/// macro-expanded code to look at without a lot of extra noise.
#[tokio::main]
async fn main() {
    join_me_maybe::join!(
        async { 1 },
        maybe async { 2 },
        label1: async { 3 },
        label2: maybe async { 4 },
    );
}
