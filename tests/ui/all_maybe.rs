use join_me_maybe::join;
use std::future::ready;

#[tokio::main]
async fn main() {
    join!(
        maybe ready(1),
        maybe ready(2),
    );
}
