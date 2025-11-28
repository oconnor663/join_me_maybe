use join_me_maybe::join_me_maybe;
use std::future::ready;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn test_maybe() {
    let ret = join_me_maybe! {
        maybe ready(1),
        definitely ready(2),
        definitely ready(3),
        maybe ready(4),
    };
    assert_eq!(ret, (Some(1), Some(2), Some(3), None));
}

#[tokio::test]
async fn test_cancel() {
    let ret = join_me_maybe! {
        maybe ready(1),
        definitely ready(2),
        foo: definitely async {
            sleep(Duration::from_secs(1_000_000)).await;
            3 // we'll never get here
        },
        maybe async {
            foo.cancel();
            4
        },
        // Without the leading underscore here you get an unused variable warning. See
        // `tests/ui/unused_label.rs`.
        _unused_label: maybe ready(5),
    };
    assert_eq!(ret, (Some(1), Some(2), None, Some(4), None));
}
