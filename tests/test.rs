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

#[tokio::test]
async fn test_cancel_already_finished() {
    let ret = join_me_maybe! {
        foo: definitely ready(0),
        definitely async {
            // `foo` will have already finished above by the time we try to cancel it here. This is
            // testing that we don't screw up the count.
            foo.cancel();
            1
        },
        definitely async {
            // Hypothetically if we screwed up the count, we might skip this arm.
            2
        },
    };
    assert_eq!(ret, (Some(0), Some(1), Some(2)));
}

#[tokio::test]
async fn test_drop_promptly() {
    let mutex = tokio::sync::Mutex::new(());
    join_me_maybe! {
        foo: definitely async {
            // Polling order is (currently) deterministic, so this arm definitely gets the lock
            // here. If that ever changes we could acquire the guard above and move it in here.
            let _guard = mutex.lock().await;
            // This arm tries to sleep "forever" while holding the lock. The other arm isn't happy
            // about that.
            sleep(Duration::from_secs(1_000_000)).await;
        },
        definitely async {
            // Take the lock from foo...by force!
            foo.cancel();
            // If cancelling `foo` doesn't drop its future promptly, this will deadlock.
            _ = mutex.lock().await;
        }
    };
}

// Most of the cases above rely on simple `ready` futures, but here we do at least one case that
// actually returns `Pending` before eventually returning `ready`.
#[tokio::test]
async fn test_nontrivial_futures() {
    let ret = join_me_maybe! {
        maybe async {
            sleep(Duration::from_millis(1)).await;
            1
        },
        definitely async {
            sleep(Duration::from_millis(10)).await;
            2
        },
    };
    assert_eq!(ret, (Some(1), Some(2)));
}
