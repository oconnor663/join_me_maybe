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
        maybe ready(0),
        definitely ready(1),
        foo: definitely async {
            sleep(Duration::from_secs(1_000_000)).await;
            2 // we'll never get here
        },
        bar: maybe async {
            sleep(Duration::from_secs(1_000_000)).await;
            3 // we'll never get here
        },
        maybe async {
            foo.cancel();
            4
        },
        definitely async {
            bar.cancel();
            5
        },
        // Without the leading underscore here you get an unused variable warning. See
        // `tests/ui/unused_label.rs`.
        _unused_label: maybe ready(6),
    };
    assert_eq!(ret, (Some(0), Some(1), None, None, Some(4), Some(5), None));
}

#[tokio::test]
async fn test_early_exit() {
    let ret = join_me_maybe! {
        maybe async {
            foo.cancel();
            // Because of this yield, we'll never get to the return value in this arm.
            sleep(Duration::from_secs(0)).await;
            0
        },
        foo: definitely ready(1),
    };
    assert_eq!(ret, (None, None));
}
