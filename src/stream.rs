use crate::channel;
use core::task::{Context, Poll};
use futures::sink::Sink;
use futures::stream::Stream;
use futures::task::AtomicWaker;
use std::io;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct Sender<T> {
    tx: channel::Sender<T>,
    rx_waker: Arc<AtomicWaker>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            tx: self.tx.clone(),
            rx_waker: self.rx_waker.clone(),
        }
    }
}

impl<T> Sink<T> for Sender<T>
where
    T: Unpin + 'static,
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

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // check if it was the last sender
        let senders = self.tx.cnts.senders.load(Ordering::SeqCst);
        // if so - close entire channel and wake up receiver
        if senders == 1 {
            self.tx.close();
            self.rx_waker.wake();
        }
    }
}

pub struct Receiver<T> {
    rx: channel::Receiver<T>,
    waker: Arc<AtomicWaker>,
}

impl<T> Receiver<T> {
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

impl<T> Stream for Receiver<T> {
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

pub fn new<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = channel::new();
    let rx_waker = Arc::new(AtomicWaker::new());
    let tx = Sender {
        tx: tx,
        rx_waker: rx_waker.clone(),
    };
    let rx = Receiver {
        rx: rx,
        waker: rx_waker,
    };
    (tx, rx)
}
