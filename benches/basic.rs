
#[macro_use]
extern crate criterion;

use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::Relaxed;
use mpmc::*;
use std::sync::Arc;
use std::thread;
use criterion::{Criterion, Bencher, black_box};
use std::time::Instant;

const VALUES: usize = 10000;
const THREADS: usize = 16;

fn channel_mpsc<S: Sender>(b: &mut Bencher) {
    b.iter_custom(|iters| {
    let (sender, receiver) = new();
    let mut counter = 0;

    let mut handles = vec![];

    for i in 0..THREADS {
        let ch = sender.clone();
        let jh = thread::spawn(move || {
            for _ in 0..VALUES {
                ch.send(i).unwrap();
            }
        });
        handles.push(jh);
    }

    let mut iters = THREADS * VALUES;

    while iters > 0 {
        match receiver.try_recv() {
            Ok(Some(v)) => {
                counter += v as usize;
                iters -= 1;
            }
            _ => {}
        }
    }

    let total = (0..THREADS).map(|i| i * VALUES).sum();

    for jh in handles.drain(..) {
        let _ = jh.join();
    }

    assert_eq!(counter, total);
}
