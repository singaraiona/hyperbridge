use crate::channel;
use core::task::{Context, Poll};
use futures::sink::Sink;
use futures::stream::Stream;
use futures::task::AtomicWaker;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

pub struct Sender<T: Send + 'static> {
    tx: channel::Sender<T>,
    rx_waker: Arc<AtomicWaker>,
}

impl<T: Send + 'static> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            tx: self.tx.clone(),
            rx_waker: self.rx_waker.clone(),
        }
    }
}

impl<T> Sink<T> for Sender<T>
where
    T: Send + Unpin + 'static,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        self.tx.send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.rx_waker.wake();
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tx.close();
        self.rx_waker.wake();
        Poll::Ready(Ok(()))
    }
}

pub struct Receiver<T: Send + 'static> {
    rx: channel::Receiver<T>,
    waker: Arc<AtomicWaker>,
}

impl<T: Send + 'static> Receiver<T> {
    #[inline]
    fn next_message(&self) -> Poll<Option<Result<T, io::Error>>> {
        match self.rx.try_recv() {
            Ok(Some(item)) => Poll::Ready(Some(Ok(item))),
            Ok(None) => Poll::Pending,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Poll::Ready(None),
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

impl<T: Send + 'static> Stream for Receiver<T> {
    type Item = Result<T, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.next_message() {
            Poll::Pending => {
                self.waker.register(cx.waker());
                self.next_message()
            }
            res => res,
        }
    }
}

pub fn new<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = channel::new();
    let rx_waker = Arc::new(AtomicWaker::new());
    let tx = Sender {
        tx: tx,
        rx_waker: rx_waker.clone(),
    };
    let rx = Receiver {
        rx: rx,
        wakers: wakers_tx,
        waker: rx_waker,
    };
    (tx, rx)
}
