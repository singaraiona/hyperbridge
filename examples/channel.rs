extern crate hyperbridge;
use std::thread;

fn main() {
    let (sender, receiver) = hyperbridge::channel::new();
    let threads = 10;
    let values = 10000;

    let mut handles = vec![];

    for i in 0..threads {
        let ch = sender.clone();
        let jh = thread::spawn(move || {
            for _ in 0..values {
                ch.send(i).unwrap();
            }
        });
        handles.push(jh);
    }

    let mut iters = threads * values;

    while iters > 0 {
        match receiver.try_recv() {
            Ok(Some(_)) => {
                iters -= 1;
            }
            _ => {}
        }
    }

    let total: usize = (0..threads).map(|i| i * values).sum();

    for jh in handles.drain(..) {
        let _ = jh.join();
    }

    println!("Send and received: {} items", total);
}
