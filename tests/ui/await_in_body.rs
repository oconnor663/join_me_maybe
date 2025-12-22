#![deny(warnings)]

use futures::stream;
use join_me_maybe::join_me_maybe;
use std::future::ready;

#[tokio::main]
async fn main() {
    let my_stream = stream::iter([3, 4, 5]);
    join_me_maybe!(
        _ = ready(1) => {
            ready(2).await
        },
        _ in stream::iter([3, 4, 5]) => {
            ready(6).await;
        },
    );
}
