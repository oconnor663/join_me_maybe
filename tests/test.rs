use futures::stream::FuturesUnordered;
use futures::{Stream, StreamExt, stream};
use join_me_maybe::join;
use pin_project_lite::pin_project;
use std::future::ready;
use std::hash::Hash;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use tokio::time::{Duration, sleep};
use tokio_stream::StreamMap;

#[tokio::test]
async fn test_maybe() {
    let ret = join!(
        maybe ready(1),
        ready(2),
        ready(3),
        maybe ready(4),
    );
    assert_eq!(ret, (Some(1), 2, 3, None));
}

#[tokio::test]
async fn test_cancel() {
    let ret = join!(
        maybe ready(0),
        ready(1),
        foo: async {
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
        async {
            bar.cancel();
            5
        },
        // Without the leading underscore here you get an unused variable warning. See
        // `tests/ui/unused_label.rs`.
        _unused_label: maybe ready(6),
    );
    assert_eq!(ret, (Some(0), 1, None, None, Some(4), 5, None));
}

#[tokio::test]
async fn test_early_exit() {
    let ret = join!(
        maybe async {
            foo.cancel();
            // Because of this yield, we'll never get to the return value in this arm.
            sleep(Duration::from_secs(0)).await;
            0
        },
        foo: ready(1),
    );
    assert_eq!(ret, (None, None));
}

#[tokio::test]
async fn test_cancel_already_finished() {
    let ret = join!(
        foo: ready(0),
        async {
            // `foo` will have already finished above by the time we try to cancel it here. This is
            // testing that we don't screw up the count.
            foo.cancel();
            1
        },
        async {
            // Hypothetically if we screwed up the count, we might skip this arm.
            2
        },
    );
    assert_eq!(ret, (Some(0), 1, 2));
}

#[tokio::test]
async fn test_cancel_self() {
    let ret = join!(
        foo: async {
            // This arm is cancelling itself, but it's going to exit anyway. Make sure we don't
            // screw up the count.
            foo.cancel();
            0
        },
        ready(1),
    );
    assert_eq!(ret, (Some(0), 1));
}

#[tokio::test]
async fn test_drop_promptly() {
    let mutex = tokio::sync::Mutex::new(());
    let ret = join!(
        foo: async {
            // Polling order is (currently) deterministic, so this arm definitely gets the lock
            // here. If that ever changes we could acquire the guard above and move it in here.
            let _guard = mutex.lock().await;
            // This arm tries to sleep "forever" while holding the lock. The other arm isn't happy
            // about that.
            sleep(Duration::from_secs(1_000_000)).await;
        },
        async {
            // Take the lock from foo...by force!
            foo.cancel();
            // If cancelling `foo` doesn't drop its future promptly, this will deadlock.
            _ = mutex.lock().await;
        }
    );
    assert_eq!(ret, (None, ()));
}

// Most of the cases above rely on simple `ready` futures, but here we do at least one case that
// actually returns `Pending` before eventually returning `ready`.
#[tokio::test]
async fn test_nontrivial_futures() {
    let ret = join!(
        maybe async {
            sleep(Duration::from_millis(1)).await;
            1
        },
        async {
            sleep(Duration::from_millis(10)).await;
            2
        },
    );
    assert_eq!(ret, (Some(1), 2));
}

#[tokio::test]
async fn test_future_arms_with_bodies() {
    let mut counter = 0;
    // Note that all of the arm bodies here can mutate `counter`.
    let ret = join!(
        maybe x = ready(1) => {
            assert_eq!(x, 1);
            counter += 1;
            "hello"
        },
        y = ready(2) => {
            counter += 1;
            10 * y
        },
        _ = ready(3) => counter += 1,
        // This arm gets cancelled.
        maybe _ = ready(4) => counter += 1,
    );
    assert_eq!(ret, (Some("hello"), 20, (), None));
    assert_eq!(counter, 3);
}

#[tokio::test]
async fn test_stream_arms() {
    let mut elements1 = Vec::new();
    let mut elements2 = Vec::new();
    let mut counter = 0;
    let ret = join!(
        x in stream::iter(0..5) => {
            elements1.push(x);
            counter+= 1;
        },
        x in stream::iter(5..8) => {
            elements2.push(x);
            counter+= 1;
        },
        _ = ready(()) => counter += 100,
    );
    assert_eq!(elements1, [0, 1, 2, 3, 4]);
    assert_eq!(elements2, [5, 6, 7]);
    assert_eq!(counter, 108);
    assert_eq!(ret, ((), (), ()));
}

fn resuming<S>(stream: S) -> ResumingStream<S> {
    ResumingStream {
        stream,
        waker: None,
        ended: false,
    }
}

pin_project! {
    struct ResumingStream<S> {
        #[pin]
        stream: S,
        waker: Option<Waker>,
        ended: bool,
    }
}

impl<S> ResumingStream<S> {
    fn inner(self: Pin<&mut Self>) -> Pin<&mut S> {
        let this = self.project();
        if let Some(waker) = this.waker {
            // Mutating the stream might mean it needs to be polled again.
            waker.wake_by_ref();
        }
        this.stream
    }

    fn end(&mut self) {
        self.ended = true;
    }
}

impl<S: Stream> Stream for ResumingStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = self.project();

        match this.stream.poll_next(cx) {
            // Once `self.ended` is set, we can let the caller observe end-of-stream.
            Poll::Ready(None) if *this.ended => Poll::Ready(None),
            // If not `self.ended`, refuse to allow the underlying stream to report that it's done.
            // Of course this causes us to poll the underlying stream again after it *tried* to
            // report that it's done, which isn't generally allowed, but `FuturesUnordered` expects
            // it.
            Poll::Pending | Poll::Ready(None) => {
                // Stash the waker so that we can request a re-poll if the inner stream is mutated.
                *this.waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Poll::Ready(Some(item)) => {
                // Wakeups are not registered unless Pending is returned. Clear the waker.
                *this.waker = None;
                Poll::Ready(Some(item))
            }
        }
    }
}

#[tokio::test]
async fn test_canceller_mut_futuresunordered() {
    let inputs = futures::stream::iter(0..5).then(|i| async move {
        sleep(Duration::from_millis(1)).await;
        i
    });
    let mut outputs = Vec::new();
    join!(
        i in inputs => {
            unordered.as_pin_mut().unwrap().inner().push(async move {
                i
            });
        } finally unordered.as_pin_mut().unwrap().end(),
        unordered: i in resuming(FuturesUnordered::new()) => outputs.push(i),
    );
    outputs.sort();
    assert_eq!(outputs, [0, 1, 2, 3, 4]);
}

// Similar to `ResumingStream` above, but more tailored to `StreamMap` specifically.
struct WellBehavedStreamMap<K, V> {
    map: StreamMap<K, V>,
    waker: Option<Waker>,
    drain: bool,
}

impl<K, V> WellBehavedStreamMap<K, V> {
    fn new() -> Self {
        Self {
            map: StreamMap::new(),
            waker: None,
            drain: false,
        }
    }

    fn start_drain(&mut self) {
        self.drain = true;
    }
}

impl<K: Hash + Eq, V: Stream> WellBehavedStreamMap<K, V> {
    fn insert(&mut self, key: K, stream: V) {
        assert!(!self.drain, "already draining");
        self.map.insert(key, stream);
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }
}

impl<K: Clone + Unpin, V: Stream + Unpin> Stream for WellBehavedStreamMap<K, V> {
    type Item = (K, V::Item);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.map).poll_next(cx) {
            Poll::Ready(None) if self.drain => {
                // Once drain is set, we can let the caller observe end-of-stream.
                Poll::Ready(None)
            }
            Poll::Pending | Poll::Ready(None) => {
                self.waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Poll::Ready(Some(item)) => {
                self.waker = None;
                Poll::Ready(Some(item))
            }
        }
    }
}

#[tokio::test]
async fn test_canceller_mut_streammap() {
    let inputs = futures::stream::iter(0..5).then(|i| async move {
        sleep(Duration::from_millis(1)).await;
        i
    });
    let mut outputs = Vec::new();
    join!(
        i in inputs => {
            stream_map.as_pin_mut().unwrap().insert(i, futures::stream::iter(vec![i; i]));
        } finally {
            stream_map.as_pin_mut().unwrap().start_drain();
        },
        stream_map: (_k, v) in WellBehavedStreamMap::new() => outputs.push(v),
    );
    outputs.sort();
    assert_eq!(outputs, [1, 2, 2, 3, 3, 3, 4, 4, 4, 4]);
}
