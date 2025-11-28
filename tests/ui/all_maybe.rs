use join_me_maybe::join_me_maybe;
use std::future::ready;

#[tokio::main]
async fn main() {
    join_me_maybe! {
        maybe ready(1),
        maybe ready(2),
    };
}
