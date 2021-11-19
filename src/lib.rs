//! # Hyperbridge
//!
//! Fast multi-producer, multi-consumer unbounded channel with async support.
//!
//! ## Example
//!
//! ```
//! let (tx, rx) = hyperbridge::channel::new();
//!
//! tx.send(42).unwrap();
//! let v = loop {
//!     match rx.try_recv() {
//!         Ok(None) => {}, // not ready
//!         Ok(Some(val)) => break val,
//!         Err(e) => panic!("channel read error: {:?}", e),
//!     }
//! };
//! assert_eq!(v, 42);
//! ```

#[cfg(feature = "with-futures")]
extern crate futures;

// mpmc channel
pub mod channel;
// Sink, Stream implementations for channel
#[cfg(feature = "with-futures")]
pub mod stream;
// auxiliary tools
pub mod util;
