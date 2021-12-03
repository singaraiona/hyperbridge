extern crate hyperbridge;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::*;
use std::sync::Arc;
use std::thread;

fn main() {
    let (sender, receiver) = hyperbridge::channel::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let threads = 128;
    let values = 10000;

    let mut handles = vec![];

    for _ in 0..threads {
        let ch = receiver.clone();
        let local_counter = counter.clone();
        let jh = thread::spawn(move || {
            let mut iters = values;
            while iters > 0 {
                if let Ok(Some(v)) = ch.try_recv() {
                    local_counter.fetch_add(v as usize, Relaxed);
                    iters -= 1;
                }
            }
        });
        handles.push(jh);
    }

    for i in 0..threads {
        let ch = sender.clone();
        let jh = thread::spawn(move || {
            for _ in 0..values {
                ch.send(i).unwrap();
            }
        });
        handles.push(jh);
    }

    for jh in handles.drain(..) {
        let _ = jh.join();
    }

    let total: usize = (0..threads).map(|i| i * values).sum();

    println!("Send and received: {} items", total);

    for i in 0..10000 {
        receiver.try_recv();
    }
}
