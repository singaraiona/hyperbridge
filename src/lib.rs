#[cfg(feature = "with-futures")]
extern crate futures;

pub mod channel;
#[cfg(feature = "with-futures")]
pub mod stream;
pub mod util;
