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
    wakers: channel::Receiver<Arc<AtomicWaker>>,
}

impl<T: Send + 'static> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            tx: self.tx.clone(),
            wakers: self.wakers.clone(),
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
        if let Ok(Some(waker)) = self.wakers.try_recv() {
            waker.wake();
        }
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tx.close();
        while let Ok(Some(waker)) = self.wakers.try_recv() {
            waker.wake();
        }
        Poll::Ready(Ok(()))
    }
}

pub struct Receiver<T: Send + 'static> {
    rx: channel::Receiver<T>,
    waker: Arc<AtomicWaker>,
    wakers: channel::Sender<Arc<AtomicWaker>>,
}

impl<T: Send + 'static> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Receiver {
            rx: self.rx.clone(),
            waker: Arc::new(AtomicWaker::new()),
            wakers: self.wakers.clone(),
        }
    }
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
        loop {
            // It's not our event, wait until our waker is waked
            if Arc::strong_count(&self.waker) > 1 {
                return Poll::Pending;
            }

            match self.next_message() {
                Poll::Ready(res) => return Poll::Ready(res),
                Poll::Pending => {
                    self.waker.register(cx.waker());
                    self.wakers.send(self.waker.clone())?;
                }
            }
        }
    }
}

pub fn new<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = channel::new();
    let (wakers_tx, wakers_rx) = channel::new();
    let tx = Sender {
        tx: tx,
        wakers: wakers_rx,
    };
    let rx = Receiver {
        rx: rx,
        wakers: wakers_tx,
        waker: Arc::new(AtomicWaker::new()),
    };
    (tx, rx)
}
