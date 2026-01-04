use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        unique1: std::future::ready(1),
        repeated: std::future::ready(2),
        repeated: std::future::ready(3),
        unique2: std::future::ready(4),
    );
}
