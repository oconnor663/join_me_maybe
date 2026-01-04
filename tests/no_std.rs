//! Just test that this function compiles in a no_std context. We don't need to actually call it.

#![no_std]

use core::future::ready;
use futures::stream;
use join_me_maybe::join;

pub async fn foo() {
    join!(
        ready(0),
        maybe ready(1),
        cancel1: _ = ready(2) => (),
        cancel2: maybe async {
            cancel1.cancel();
        },
        _ in stream::iter([3, 4, 5]) => (),
    );
}
