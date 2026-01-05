use join_me_maybe::join;
use tokio::time::{Duration, sleep};

fn assert_send<T: Send>(_: &T) {}

#[tokio::main]
async fn main() {
    join!(
        foo: async {
            // This non-Send `Rc` makes this future non-Send, which should make its `Canceller`
            // non-Send too.
            let _x = std::rc::Rc::new(());
            sleep(Duration::from_millis(1)).await;
            drop(_x);
        },
        async {
            // This should fail to compile.
            assert_send(&foo);
        },
    );
}
