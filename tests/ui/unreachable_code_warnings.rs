#![deny(unreachable_code)]

use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        _ = async {} => {
            panic!();
            _ = 42;
        },
        _ in futures::stream::iter([42]) => {
            panic!();
            _ = 42;
        } finally {
            panic!();
            _ = 42;
        },
    );
}
