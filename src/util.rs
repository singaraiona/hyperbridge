use core::ops::{Deref, DerefMut};
use std::cell::Cell;

const SPIN_LIMIT: u32 = 6;
const YIELD_LIMIT: u32 = 10;

#[repr(transparent)]
#[derive(Debug)]
pub struct Backoff {
    rounds: Cell<u32>,
}

impl Backoff {
    #[inline]
    pub fn new() -> Self {
        Backoff {
            rounds: Cell::new(0),
        }
    }

    #[inline]
    pub fn reset(&self) {
        self.rounds.set(0)
    }

    #[inline]
    pub fn rounds(&self) -> u32 {
        self.rounds.get()
    }

    pub const fn spin_limit(&self) -> u32 {
        SPIN_LIMIT
    }

    pub const fn yield_limit(&self) -> u32 {
        YIELD_LIMIT
    }

    #[inline]
    pub fn spin_once(&self) {
        std::hint::spin_loop();
    }

    #[inline]
    pub fn spin(&self) {
        for _ in 0..1 << self.rounds.get().min(SPIN_LIMIT) {
            std::hint::spin_loop();
        }

        if self.rounds.get() <= SPIN_LIMIT {
            self.rounds.set(self.rounds.get() + 1);
        }
    }

    #[inline]
    pub fn snooze(&self) {
        if self.rounds.get() <= SPIN_LIMIT {
            for _ in 0..1 << self.rounds.get().min(SPIN_LIMIT) {
                std::hint::spin_loop();
            }
        } else {
            std::thread::yield_now();
        }

        if self.rounds.get() <= YIELD_LIMIT {
            self.rounds.set(self.rounds.get() + 1);
        }
    }
}

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), repr(align(128)))]
#[cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    repr(align(64))
)]
#[derive(Debug)]
pub struct CachePadded<T>(T);

impl<T> CachePadded<T> {
    pub const fn new(t: T) -> CachePadded<T> {
        CachePadded(t)
    }
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
