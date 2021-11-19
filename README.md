# Hyperbridge

Fast multi-producer, multi-consumer unbounded channel with async support. Inspired by [crossbeam unbounded channel](https://github.com/crossbeam-rs/crossbeam).

[![Cargo](https://img.shields.io/crates/v/hyperbridge.svg)](
https://crates.io/crates/hyperbridge)
[![Documentation](https://docs.rs/hyperbridge/badge.svg)](
https://docs.rs/hyperbridge)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](
https://github.com/zesterer/hyperbridge)

## Examples

### `Hyperbridge::channel`: mpsc

```rust
use hyperbridge::channel;
use std::thread;

let (sender, receiver) = hyperbridge::channel::new();
let mut counter = 0;
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
        Ok(Some(v)) => {
            counter += v as usize;
            iters -= 1;
        }
        _ => {}
    }
}

let total = (0..threads).map(|i| i * values).sum();

for jh in handles.drain(..) {
    let _ = jh.join();
}
```

### `Hyperbridge::channel`: mpmc

```rust
use hyperbridge::channel;
use std::thread;

const VALUES: usize = 10000;
const THREADS: usize = 16;

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
```

## Dependencies

```toml
# Cargo.toml
[dependencies]
hyperbridge = "0.1.0"
```

## Feature with-futures

Turns on support for futures Sink, Stream:

```toml
# Cargo.toml
[dependencies]
hyperbridge = { version = "0.1.0", features = ["with-futures"] }
```

## License

MIT/Apache-2.0
