use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        maybe std::future::ready(1),
        maybe std::future::ready(2),
    );
}
