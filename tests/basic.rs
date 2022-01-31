#![feature(test)]
extern crate crossbeam;
extern crate test;

use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::Relaxed;
use hyperbridge::channel;
use std::sync::Arc;
use std::thread;

const VALUES: usize = 10000;
const THREADS: usize = 16;

#[test]
fn hyperbridge_close() {
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

    for jh in handles.drain(..) {
        let _ = jh.join();
    }

    sender.close();

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

    assert_eq!(counter, total);
}

#[test]
fn hyperbridge_mpsc() {
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

    let total = (0..THREADS).map(|i| i * VALUES).sum();

    for jh in handles.drain(..) {
        let _ = jh.join();
    }

    assert_eq!(counter, total);
}

#[test]
fn hyperbridge_mpmc() {
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

    let total = (0..THREADS).map(|i| i * VALUES).sum();

    assert_eq!(counter.load(Relaxed), total);
}

#[test]
fn hyperbridge_drop_senders() {
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

    for jh in handles.drain(..) {
        let _ = jh.join();
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

    assert_eq!(counter, total);

    // should call close on drop last sender
    drop(sender);

    match receiver.try_recv() {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe),
        _ => panic!("expected closed channel"),
    }
}

#[test]
fn hyperbridge_drop_receiver() {
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

    for jh in handles.drain(..) {
        let _ = jh.join();
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

    assert_eq!(counter, total);

    // should call close on drop last receiver
    drop(receiver);

    match sender.send(0) {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe),
        _ => panic!("expected closed channel"),
    }
}
