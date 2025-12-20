//! This module was originally copied from the `futures` crate, but it's been modified a lot.
//!
//! <https://github.com/rust-lang/futures-rs/blob/de9274e655b2fff8c9630a259a473b71a6b79dda/futures-util/src/future/maybe_done.rs>

use core::future::Future;
use core::mem;
use core::pin::Pin;
use core::task::{Context, Poll};

pub enum MaybeDone<Fut: Future, T> {
    Future(/* #[pin] */ Fut),
    Done(T),
    Gone,
}

impl<Fut: Future, T> MaybeDone<Fut, T> {
    #[inline]
    pub fn is_future(&self) -> bool {
        matches!(*self, Self::Future(_))
    }

    #[inline]
    pub fn take_output(self: Pin<&mut Self>) -> Option<T> {
        match &*self {
            Self::Done(_) => {}
            Self::Future(_) | Self::Gone => return None,
        }
        unsafe {
            match mem::replace(self.get_unchecked_mut(), Self::Gone) {
                Self::Done(output) => Some(output),
                _ => core::hint::unreachable_unchecked(),
            }
        }
    }

    // In the common case (`foo` below), `map` will be the identity function, but it can also be
    // explicitly provided (`bar` below):
    //
    // ```
    // join_me_maybe!(
    //     foo(),
    //     x = bar() => {
    //         x + 1
    //     }
    //     ...
    // );
    // ```
    #[inline]
    pub fn poll_map(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        map: impl FnOnce(Fut::Output) -> T,
    ) -> Poll<()> {
        unsafe {
            match self.as_mut().get_unchecked_mut() {
                Self::Future(fut) => {
                    let Poll::Ready(output) = Pin::new_unchecked(fut).poll(cx) else {
                        return Poll::Pending;
                    };
                    self.set(Self::Done(map(output)));
                    Poll::Ready(())
                }
                _ => panic!("MaybeDone polled again after completion"),
            }
        }
    }
}
