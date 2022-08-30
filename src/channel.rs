use crate::util::{Backoff, CachePadded};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use core::sync::atomic::{fence, AtomicPtr, AtomicUsize};
use std::sync::Arc;
use std::{fmt, io};

// Slot states
const WRITE: usize = 1;
const READ: usize = 1 << 1;
const DESTROY: usize = 1 << 2;
// Read/Write round
const ROUND: usize = 64;
// Block capacity
const BLOCK_SIZE: usize = ROUND - 1;
// In a tail means that channel has been closed
const CLOSED_FLAG: usize = 1 << 63;
// In a head means head and tail are in a different blocks
const CROSSED_FLAG: usize = CLOSED_FLAG;

/// A place for storing a message
struct Slot<T> {
    state: AtomicUsize,
    message: UnsafeCell<MaybeUninit<T>>,
}

impl<T> fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("state", &self.state.load(Acquire))
            .finish()
    }
}

/// A block in a linked list.
struct Block<T> {
    next: AtomicPtr<Block<T>>,
    slots: [Slot<T>; BLOCK_SIZE],
}

impl<T> Block<T> {
    /// Creates new empty block
    fn new() -> Self {
        unsafe { MaybeUninit::zeroed().assume_init() }
    }

    /// Blocks current thread waiting the next block is set
    fn wait_next(&self) -> *mut Block<T> {
        let backoff = Backoff::new();
        loop {
            let next = self.next.load(Acquire);
            if !next.is_null() {
                return next;
            }
            backoff.spin();
        }
    }

    /// Stores new one allocated block inside `next`
    fn set_next(&self, next: *mut Block<T>) {
        let prev = self.next.swap(next, Release);
        debug_assert!(prev.is_null());
    }

    /// Returns block inside `next`
    fn get_next(&self) -> Option<*mut Block<T>> {
        let next = self.next.load(Acquire);
        if next.is_null() {
            return None;
        }
        Some(next)
    }

    /// Drop block if there are no readers using this block remains or leave dropping to a next thread
    fn destroy(this: *mut Block<T>, start: usize) {
        // we can skip marking the last block with DESTROY because it has started destroy process
        for i in start..BLOCK_SIZE - 1 {
            let slot = unsafe { (*this).slots.get_unchecked(i) };

            // set DESTROY bit if someone is still reading from this slot.
            if slot.state.load(Acquire) & READ == 0
                && slot.state.fetch_or(DESTROY, AcqRel) & READ == 0
            {
                // if someone is still using the slot, it will continue destruction of the block.
                return;
            }
        }

        // noone is using the block, now it is safe to destroy it.
        unsafe { drop(Box::from_raw(this)) };
    }
}

/// A pointer to a block/slot
#[derive(Debug)]
struct Cursor<T> {
    index: AtomicUsize,
    block: AtomicPtr<Block<T>>,
}

impl<T> Cursor<T> {
    fn from(block: *mut Block<T>) -> Self {
        Cursor {
            index: AtomicUsize::new(0),
            block: AtomicPtr::new(block),
        }
    }

    #[inline]
    fn slot<'a>(block: *mut Block<T>, index: usize) -> &'a Slot<T> {
        debug_assert!(index < BLOCK_SIZE);
        unsafe { (*block).slots.get_unchecked(index) }
    }
}

/// Unbounded channel
#[derive(Debug)]
struct Channel<T> {
    tail: CachePadded<Cursor<T>>,
    head: CachePadded<Cursor<T>>,
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        // read all unread items to drop correctly
        while let Ok(Some(_)) = self.try_recv() {}

        let block = self.head.block.load(Acquire);

        // noone is using the block, now it is safe to destroy it.
        unsafe { drop(Box::from_raw(block)) };
    }
}

impl<T> Channel<T> {
    /// Creates new unbounded channel
    fn new() -> Channel<T> {
        let block = Block::<T>::new();
        let block = Box::into_raw(Box::new(block));
        Channel {
            tail: CachePadded::new(Cursor::from(block)),
            head: CachePadded::new(Cursor::from(block)),
        }
    }

    /// Try to send a message to a channel.
    /// Can not fail but when channel is closed
    #[inline]
    fn send(&self, msg: T) -> io::Result<()> {
        let backoff = Backoff::new();
        let mut tail = self.tail.index.load(Acquire);
        let mut block = self.tail.block.load(Acquire);
        let mut next_block: Option<Block<T>> = None;

        loop {
            let index = tail & BLOCK_SIZE;

            // channel is closed
            if tail & CLOSED_FLAG == CLOSED_FLAG {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel is closed",
                ));
            }

            // wait next block
            if index == BLOCK_SIZE {
                backoff.snooze();
                tail = self.tail.index.load(Acquire);
                block = self.tail.block.load(Acquire);
                continue;
            }

            // End of block, need to setup new one as soon as possible to reduce waiting other writing threads
            if index + 1 == BLOCK_SIZE && next_block.is_none() {
                next_block = Some(Block::new());
            }

            // try to move tail forward
            match self
                .tail
                .index
                .compare_exchange_weak(tail, tail + 1, SeqCst, Relaxed)
            {
                Ok(_) => {
                    let slot = Cursor::slot(block, index);

                    // End of block, need to setup new one
                    if index + 1 == BLOCK_SIZE {
                        let next = Box::into_raw(Box::new(next_block.unwrap()));
                        self.tail.block.store(next, Release);
                        self.tail.index.fetch_add(1, Release);
                        unsafe { (*block).set_next(next) };
                    }

                    unsafe { slot.message.get().write(MaybeUninit::new(msg)) };
                    slot.state.fetch_or(WRITE, Release);
                    return Ok(());
                }
                Err(t) => {
                    tail = t;
                    block = self.tail.block.load(Acquire);
                    backoff.spin();
                }
            }
        }
    }

    /// Try to receive message from a channel.
    /// Can fail or return uncompleted.
    #[inline]
    fn try_recv(&self) -> io::Result<Option<T>> {
        let backoff = Backoff::new();
        let mut head = self.head.index.load(Acquire);
        let mut block = self.head.block.load(Acquire);

        loop {
            let index = head & BLOCK_SIZE;

            // wait next block
            if index == BLOCK_SIZE {
                backoff.snooze();
                head = self.head.index.load(Acquire);
                block = self.head.block.load(Acquire);
                continue;
            }

            let mut new_head = head + 1;

            // head and tail are in the same block
            if head & CROSSED_FLAG == 0 {
                fence(SeqCst);
                let tail = self.tail.index.load(Relaxed);

                // Nothing to read
                if head == (tail & !CLOSED_FLAG) {
                    // channel is closed
                    if tail & CROSSED_FLAG != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "channel is closed",
                        ));
                    }
                    return Ok(None);
                }

                // mark head that it is in a different blocks with tail
                if head / ROUND != (tail & !CLOSED_FLAG) / ROUND {
                    new_head |= CROSSED_FLAG;
                }
            }

            // try to move head forward
            match self
                .head
                .index
                .compare_exchange_weak(head, new_head, SeqCst, Relaxed)
            {
                Ok(_) => unsafe {
                    // last slot in a block
                    if index + 1 == BLOCK_SIZE {
                        let next_block = (*block).wait_next();
                        if (*next_block).get_next().is_some() {
                            new_head |= CROSSED_FLAG;
                        } else {
                            new_head &= !CROSSED_FLAG;
                        }
                        self.head.block.store(next_block, Release);
                        self.head.index.store(new_head + 1, Release);
                    }

                    let slot = Cursor::slot(block, index);

                    // wait until write operation completes
                    while slot.state.load(Acquire) & WRITE == 0 {
                        backoff.spin();
                    }

                    let msg = slot.message.get().read().assume_init();

                    // this is the last block, so start destroying it
                    if index + 1 == BLOCK_SIZE {
                        Block::destroy(block, 0);
                    }
                    // someone started block destroy
                    else if slot.state.fetch_or(READ, AcqRel) & DESTROY != 0 {
                        Block::destroy(block, index + 1);
                    }

                    return Ok(Some(msg));
                },

                Err(h) => {
                    head = h;
                    block = self.head.block.load(Acquire);
                    backoff.spin();
                }
            }
        }
    }

    /// Check if there are no mesages in a channel
    #[inline]
    fn is_empty(&self) -> bool {
        let head = self.head.index.load(SeqCst);
        let tail = self.tail.index.load(SeqCst);
        head & !CROSSED_FLAG == tail & !CLOSED_FLAG
    }

    // Closes a channel
    #[inline]
    fn close(&self) {
        self.tail.index.fetch_or(CLOSED_FLAG, AcqRel);
    }
}

// Helpers struct holds together receivers and senders rc's
// for handling drops of each side of channel (all senders or all receivers)
// and calling close on entire channel for that
pub(crate) struct Counters {
    pub(crate) receivers: AtomicUsize,
    pub(crate) senders: AtomicUsize,
}

/// Tx handle to a channel. Can be cloned
pub struct Sender<T> {
    chan: Arc<Channel<T>>,
    pub(crate) cnts: Arc<Counters>,
}

impl<T: fmt::Debug> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.chan)
    }
}

impl<T> Sender<T> {
    #[inline]
    pub fn send(&self, item: T) -> io::Result<()> {
        self.chan.send(item)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.chan.is_empty()
    }

    pub fn close(&self) {
        self.chan.close()
    }

    pub fn receiver(&self) -> Receiver<T> {
        self.cnts.receivers.fetch_add(1, SeqCst);
        Receiver {
            chan: Arc::clone(&self.chan),
            cnts: self.cnts.clone(),
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.cnts.senders.fetch_add(1, SeqCst);
        Sender {
            chan: Arc::clone(&self.chan),
            cnts: self.cnts.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let senders = self.cnts.senders.fetch_sub(1, SeqCst);
        // if it was the last sender - close channel
        if senders == 1 {
            self.chan.close();
        }
    }
}

unsafe impl<T> Send for Sender<T> {}

/// Rx handle to a channel. Can be cloned
pub struct Receiver<T> {
    chan: Arc<Channel<T>>,
    pub(crate) cnts: Arc<Counters>,
}

impl<T> Receiver<T> {
    #[inline]
    pub fn try_recv(&self) -> io::Result<Option<T>> {
        self.chan.try_recv()
    }

    pub fn sender(&self) -> Sender<T> {
        self.cnts.senders.fetch_add(1, SeqCst);
        Sender {
            chan: Arc::clone(&self.chan),
            cnts: self.cnts.clone(),
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.cnts.receivers.fetch_add(1, SeqCst);
        Receiver {
            chan: Arc::clone(&self.chan),
            cnts: self.cnts.clone(),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let receivers = self.cnts.receivers.fetch_sub(1, SeqCst);
        // if it was the last receiver - close channel
        if receivers == 1 {
            self.chan.close();
        }
    }
}

unsafe impl<T> Send for Receiver<T> {}

/// Creates a new channel and splits it ro a Tx, Rx pair
pub fn new<T>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Channel::new());
    let cnts = Arc::new(Counters {
        receivers: AtomicUsize::new(1),
        senders: AtomicUsize::new(1),
    });
    let tx = Sender {
        chan: chan.clone(),
        cnts: cnts.clone(),
    };
    let rx = Receiver {
        chan: chan.clone(),
        cnts: cnts,
    };
    (tx, rx)
}
