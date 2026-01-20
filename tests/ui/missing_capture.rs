use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        async {} => (),
    );
}
