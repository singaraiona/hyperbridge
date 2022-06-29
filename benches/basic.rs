#[macro_use]
extern crate criterion;

use criterion::{Bencher, Criterion};
use hyperbridge::channel;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::thread;

const VALUES: usize = 10000;
const THREADS: usize = 16;

fn test_hyperbridge_mpsc(b: &mut Bencher) {
    b.iter(|| {
        let (sender, receiver) = channel::new();
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

        let total: usize = (0..THREADS).map(|i| i * VALUES).sum();

        for jh in handles.drain(..) {
            let _ = jh.join();
        }

        assert_eq!(counter, total);
    });
}

fn test_crossbeam_mpsc(b: &mut Bencher) {
    b.iter(|| {
        let (sender, receiver) = crossbeam::channel::unbounded();
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
                Ok(v) => {
                    counter += v as usize;
                    iters -= 1;
                }
                _ => {}
            }
        }

        let total: usize = (0..THREADS).map(|i| i * VALUES).sum();

        for jh in handles.drain(..) {
            let _ = jh.join();
        }

        assert_eq!(counter, total);
    });
}

fn test_hyperbridge_mpmc(b: &mut Bencher) {
    b.iter(|| {
        let (sender, receiver) = channel::new();
        let counter = Arc::new(AtomicUsize::new(0));

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

        for _ in 0..THREADS {
            let ch = receiver.clone();
            let local_counter = counter.clone();
            let jh = thread::spawn(move || {
                let mut iters = VALUES;
                while iters > 0 {
                    if let Ok(Some(v)) = ch.try_recv() {
                        local_counter.fetch_add(v as usize, Relaxed);
                        iters -= 1;
                    }
                }
            });
            handles.push(jh);
        }

        for jh in handles.drain(..) {
            let _ = jh.join();
        }

        let total: usize = (0..THREADS).map(|i| i * VALUES).sum();

        assert_eq!(counter.load(Relaxed), total);
    });
}

fn test_crossbeam_mpmc(b: &mut Bencher) {
    b.iter(|| {
        let (sender, receiver) = crossbeam::channel::unbounded();
        let counter = Arc::new(AtomicUsize::new(0));

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

        for _ in 0..THREADS {
            let ch = receiver.clone();
            let local_counter = counter.clone();
            let jh = thread::spawn(move || {
                let mut iters = VALUES;
                while iters > 0 {
                    if let Ok(v) = ch.try_recv() {
                        local_counter.fetch_add(v as usize, Relaxed);
                        iters -= 1;
                    }
                }
            });
            handles.push(jh);
        }

        for jh in handles.drain(..) {
            let _ = jh.join();
        }

        let total: usize = (0..THREADS).map(|i| i * VALUES).sum();

        assert_eq!(counter.load(Relaxed), total);
    });
}

fn hyperbridge_channel(b: &mut Criterion) {
    b.bench_function("hyperbridge-mpsc", |b| test_hyperbridge_mpsc(b));
    // b.bench_function("hyperbridge-mpmc", |b| test_hyperbridge_mpmc(b));
}

fn crossbeam_channel(b: &mut Criterion) {
    b.bench_function("crossbeam-mpsc", |b| test_crossbeam_mpsc(b));
    // b.bench_function("crossbeam-mpmc", |b| test_crossbeam_mpmc(b));
}

criterion_group!(compare, hyperbridge_channel, crossbeam_channel);
criterion_main!(compare);
