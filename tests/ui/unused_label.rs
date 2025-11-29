#![deny(warnings)]

use join_me_maybe::join_me_maybe;
use std::future::ready;

#[tokio::main]
async fn main() {
    join_me_maybe! {
        ready(1),
        unused_label: maybe ready(2),
    };
}
