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
    pub fn is_finished(&self) -> bool {
        !matches!(*self, Self::Future(_))
    }

    #[inline]
    pub fn cancel_if_not_finished(mut self: Pin<&mut Self>) {
        if !self.is_finished() {
            self.set(Self::Gone);
        }
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
        cx: &mut Context,
        // TODO: In practice the compiler isn't going to be able to see that we only ever execute
        // these once. We always instantiate the `map` closure at the point where we call
        // `poll_map`, which is what lets each closure get exclusive mutable access to the
        // enclosing scope, but that also means the compiler can't see that the body only executes
        // once. I don't think Rust has a way to represent multiple blocks of code which execute in
        // some sequence (so they can mutate shared state) and which also execute only once (so
        // they can move/consume unshared state).
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

pub enum MaybeDoneStream<S: futures::Stream> {
    Stream(/* #[pin] */ S),
    Gone,
}

impl<S: futures::Stream> MaybeDoneStream<S> {
    #[inline]
    pub fn is_finished(&self) -> bool {
        !matches!(*self, Self::Stream(_))
    }

    #[inline]
    pub fn cancel_if_not_finished(mut self: Pin<&mut Self>) {
        if !self.is_finished() {
            self.set(Self::Gone);
        }
    }

    // Similar to `poll_map` above, but for streams instead of futures. Yielded items get passed to
    // the caller's `for_each` closure. One call to `poll_for_each` consumes as many items as it
    // can, until it encounters `Pending` or `Ready(None)`.
    #[inline]
    pub fn poll_for_each(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        mut for_each: impl FnMut(S::Item),
    ) -> Poll<()> {
        unsafe {
            loop {
                match self.as_mut().get_unchecked_mut() {
                    Self::Stream(s) => match Pin::new_unchecked(s).poll_next(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Some(item)) => for_each(item),
                        Poll::Ready(None) => {
                            self.set(Self::Gone);
                            return Poll::Ready(());
                        }
                    },
                    Self::Gone => panic!("MaybeDoneStream polled again after completion"),
                }
            }
        }
    }
}
