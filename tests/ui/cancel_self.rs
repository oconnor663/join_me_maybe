use join_me_maybe::join;

#[tokio::main]
async fn main() {
    join!(
        async {
            // `foo` is in scope for this arm, and the last one below.
            foo.cancel();
        },
        foo: async {
            // But `foo` isn't in its own scope.
            foo.cancel();
            0
        },
        async {
            foo.cancel();
        },
    );
}
