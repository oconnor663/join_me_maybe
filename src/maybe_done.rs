//! This file was originally copied from the `futures` crate.
//!
//! <https://github.com/rust-lang/futures-rs/blob/de9274e655b2fff8c9630a259a473b71a6b79dda/futures-util/src/future/maybe_done.rs>

use core::future::Future;
use core::mem;
use core::pin::Pin;
use core::task::{Context, Poll};

/// A future that may have completed.
///
/// This is created by the [`maybe_done()`] function.
pub enum MaybeDone<Fut: Future> {
    /// A not-yet-completed future
    Future(/* #[pin] */ Fut),
    /// The output of the completed future
    Done(Fut::Output),
    /// The empty variant after the result of a [`MaybeDone`] has been
    /// taken using the [`take_output`](MaybeDone::take_output) method.
    Gone,
}

impl<Fut: Future + Unpin> Unpin for MaybeDone<Fut> {}

pub fn maybe_done<Fut: Future>(future: Fut) -> MaybeDone<Fut> {
    MaybeDone::Future(future)
}

impl<Fut: Future> MaybeDone<Fut> {
    #[inline]
    pub fn is_future(&self) -> bool {
        matches!(*self, Self::Future(_))
    }

    #[inline]
    pub fn take_output(self: Pin<&mut Self>) -> Option<Fut::Output> {
        match &*self {
            Self::Done(_) => {}
            Self::Future(_) | Self::Gone => return None,
        }
        unsafe {
            match mem::replace(self.get_unchecked_mut(), Self::Gone) {
                Self::Done(output) => Some(output),
                _ => unreachable!(),
            }
        }
    }
}

impl<Fut: Future> Future for MaybeDone<Fut> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            match self.as_mut().get_unchecked_mut() {
                Self::Future(f) => {
                    let Poll::Ready(res) = Pin::new_unchecked(f).poll(cx) else {
                        return Poll::Pending;
                    };
                    self.set(Self::Done(res));
                }
                Self::Done(_) => {}
                Self::Gone => panic!("MaybeDone polled after value taken"),
            }
        }
        Poll::Ready(())
    }
}
