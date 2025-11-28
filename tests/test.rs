use join_me_maybe::join_me_maybe;
use std::future::ready;

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
        definitely async {
            cancel();
            2
        },
        definitely ready(3),
        maybe ready(4),
    };
    assert_eq!(ret, (None, None, None, None));
}

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
