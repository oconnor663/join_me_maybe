//! Just test that this function compiles in a no_std context. We don't need to actually call it.

#![no_std]

use core::future::ready;
use join_me_maybe::join_me_maybe;

pub async fn foo() {
    join_me_maybe! {
        definitely ready(0),
        maybe ready(1),
        cancel1: definitely ready(2),
        cancel2: maybe async {
            cancel1.cancel();
            cancel2.cancel();
        }
    };
}
