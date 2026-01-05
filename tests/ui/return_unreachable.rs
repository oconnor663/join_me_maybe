#![deny(unreachable_code)]

use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        _ = std::future::ready(()) => {
            return;
            // This line should be an unreachable warning.
            println!();
        },
        _ = std::future::ready(()) => {
            println!();
            return;
            // But there shouldn't be any warnings here.
        },
    );
}
