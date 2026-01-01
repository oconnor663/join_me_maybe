#![deny(warnings)]

use futures::stream;
use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        _ in stream::iter([42]) => 99,
    );
}
