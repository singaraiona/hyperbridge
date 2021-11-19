use crate::util::{Backoff, CachePadded};
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use core::sync::atomic::{AtomicPtr, AtomicUsize};
use std::sync::Arc;
use std::{fmt, io};

// slot states
const OCCUPIED: usize = 1;
const READ: usize = 1 << 1;
const DESTROY: usize = 1 << 3;
// index offset inside each block
const INDEX_SHIFT: usize = 48;
// indicates that channel was closed
const CLOSED_FLAG: usize = 1;
// indicates that head and tail are in the same block
const SAME_BLOCK_FLAG: usize = 1 << 1;
const FLAGS: usize = CLOSED_FLAG | SAME_BLOCK_FLAG;
const INDEX_MASK: usize = (usize::MAX << INDEX_SHIFT) & !FLAGS;
const BLOCK_MASK: usize = !(INDEX_MASK | FLAGS);
const BLOCK_SIZE: usize = 64;

struct Slot<T: Send> {
    state: AtomicUsize,
    message: UnsafeCell<MaybeUninit<T>>,
}

impl<T: Send> fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("state", &self.state.load(Acquire))
            .finish()
    }
}

struct Block<T: Send> {
    next: AtomicPtr<Block<T>>,
    slots: [Slot<T>; BLOCK_SIZE],
}

impl<T: Send> Block<T> {
    fn new() -> *mut Block<T> {
        let block: Block<T> = unsafe { MaybeUninit::zeroed().assume_init() };
        Box::into_raw(Box::new(block))
    }

    fn wait_next(&self) -> *mut Block<T> {
        let backoff = Backoff::new();
        loop {
            let next = self.next.load(Acquire);
            if !next.is_null() {
                return next;
            }
            backoff.snooze();
        }
    }

    fn next(&self) -> *mut Block<T> {
        let next = self.next.load(Acquire);
        if next.is_null() {
            let next = Self::new();
            self.next.store(next, Release);
            return next;
        }
        next
    }

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

struct Position<T: Send> {
    block: *mut Block<T>,
    index: usize,
    flags: usize,
}

impl<T: Send> fmt::Debug for Position<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Position")
            .field("block", &self.block)
            .field("index", &self.index)
            .finish()
    }
}

impl<T: Send> Position<T> {
    #[inline]
    fn unpack(val: usize) -> Self {
        let block = (val & BLOCK_MASK) as *mut Block<T>;
        let index = (val & INDEX_MASK) >> INDEX_SHIFT;
        let flags = val & FLAGS;
        Position {
            block,
            index,
            flags,
        }
    }

    #[inline]
    fn pack(&self) -> usize {
        self.block as usize | (self.index << INDEX_SHIFT) | self.flags
    }

    #[inline]
    fn increment(&self) -> Self {
        Position {
            block: self.block,
            index: self.index + 1,
            flags: self.flags,
        }
    }

    #[inline]
    fn slot(&self) -> &Slot<T> {
        unsafe { &*(*self.block).slots.get_unchecked(self.index) }
    }
}

#[repr(transparent)]
struct Cursor<T: Send> {
    inner: AtomicUsize,
    _phantom: PhantomData<T>,
}

impl<T: Send> Cursor<T> {
    #[inline]
    fn from(position: Position<T>) -> Self {
        debug_assert!(position.block as usize & !BLOCK_MASK == 0);
        Cursor {
            inner: AtomicUsize::new(position.block as usize | position.index << INDEX_SHIFT),
            _phantom: PhantomData,
        }
    }
}

impl<T: Send> Deref for Cursor<T> {
    type Target = AtomicUsize;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct Channel<T: Send> {
    tail: CachePadded<Cursor<Block<T>>>,
    head: CachePadded<Cursor<Block<T>>>,
}

impl<T: Send> Drop for Channel<T> {
    fn drop(&mut self) {
        // read all unread items to drop correctly
        while let Ok(Some(_)) = self.try_recv() {}

        let head_packed = self.head.load(Acquire);
        let head: Position<T> = Position::unpack(head_packed);

        // noone is using the block, now it is safe to destroy it.
        unsafe { drop(Box::from_raw(head.block)) };
    }
}

unsafe impl<T: Send> Sync for Channel<T> {}
unsafe impl<T: Send> Send for Channel<T> {}

impl<T: Send> Channel<T> {
    fn new() -> Channel<T> {
        let block = Block::<T>::new();
        Channel {
            tail: CachePadded::new(Cursor::from(Position::unpack(block as usize))),
            head: CachePadded::new(Cursor::from(Position::unpack(block as usize))),
        }
    }

    #[inline]
    fn send(&self, msg: T) -> io::Result<()> {
        let backoff = Backoff::new();
        let mut tail_packed = self.tail.load(Acquire);

        loop {
            let tail: Position<T> = Position::unpack(tail_packed);

            // channel is closed
            if tail.flags & CLOSED_FLAG == CLOSED_FLAG {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel is closed",
                ));
            }

            // wait next block
            if tail.index == BLOCK_SIZE {
                backoff.snooze();
                tail_packed = self.tail.load(Acquire);
                continue;
            }

            // try to move tail forward
            match self.tail.compare_exchange_weak(
                tail_packed,
                tail.increment().pack(),
                SeqCst,
                Relaxed,
            ) {
                Ok(_) => {
                    let slot = tail.slot();

                    // End of block, need to setup new one
                    if tail.index + 1 == BLOCK_SIZE {
                        let new_tail_packed = unsafe { (*tail.block).next() as usize };
                        self.tail.store(new_tail_packed, Release);
                    }

                    unsafe { slot.message.get().write(MaybeUninit::new(msg)) };
                    slot.state.store(OCCUPIED, Release);
                    return Ok(());
                }
                Err(t) => {
                    tail_packed = t;
                    backoff.spin();
                    continue;
                }
            }
        }
    }

    #[inline]
    fn try_recv(&self) -> io::Result<Option<T>> {
        let backoff = Backoff::new();
        let mut head_packed = self.head.load(Acquire);

        loop {
            let mut head: Position<T> = Position::unpack(head_packed);

            // wait next block
            if head.index == BLOCK_SIZE {
                backoff.snooze();
                head_packed = self.head.load(Acquire);
                continue;
            }

            // channel is closed
            if head.flags & CLOSED_FLAG == CLOSED_FLAG {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel is closed",
                ));
            }

            // head and tail are in the same block
            if head.flags & SAME_BLOCK_FLAG == 0 {
                let tail_packed = self.tail.load(Acquire);

                // Nothing to read
                if head_packed == tail_packed {
                    return Ok(None);
                }

                let tail: Position<T> = Position::unpack(tail_packed);
                if head.block != tail.block {
                    head.flags |= SAME_BLOCK_FLAG;
                }
            }

            let slot = head.slot();

            // try to move head forward
            match self.head.compare_exchange_weak(
                head_packed,
                head.increment().pack(),
                SeqCst,
                Relaxed,
            ) {
                Ok(_) => {
                    // last slot in a block
                    if head.index + 1 == BLOCK_SIZE {
                        let next_block_ptr = unsafe { (*head.block).wait_next() };
                        self.head.store(next_block_ptr as usize, Release);
                    }

                    // wait until write operation completes
                    while slot.state.load(Acquire) & OCCUPIED == 0 {
                        backoff.spin();
                    }

                    let msg = unsafe { slot.message.get().read().assume_init() };

                    // this is the last block, so start destroying it
                    if head.index + 1 == BLOCK_SIZE {
                        Block::destroy(head.block, 0);
                    }
                    // someone started block destroy
                    else if slot.state.fetch_or(READ, AcqRel) & DESTROY != 0 {
                        Block::destroy(head.block, head.index + 1);
                    }

                    return Ok(Some(msg));
                }

                Err(h) => {
                    head_packed = h;
                    backoff.spin();
                    continue;
                }
            }
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        let head = self.head.load(SeqCst);
        let tail = self.tail.load(SeqCst);
        head == tail
    }

    #[inline]
    fn close(&self) {
        self.tail.fetch_or(CLOSED_FLAG, AcqRel);
        self.head.fetch_or(CLOSED_FLAG, AcqRel);
    }
}

impl<T: Send> fmt::Debug for Channel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let head: Position<Slot<T>> = Position::unpack(self.head.load(Acquire));
        let tail: Position<Slot<T>> = Position::unpack(self.tail.load(Acquire));

        let slot = head.slot();
        let state = slot.state.load(Acquire);

        f.debug_struct("Channel")
            .field("head", &head)
            .field("tail", &tail)
            .field("slot", &state)
            .finish()
    }
}

pub struct Sender<T: Send + 'static> {
    chan: Arc<Channel<T>>,
}

impl<T: Send> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.chan)
    }
}

impl<T: Send + 'static> Sender<T> {
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
}

impl<T: Send + 'static> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            chan: Arc::clone(&self.chan),
        }
    }
}

unsafe impl<T: Send + 'static> Send for Sender<T> {}

pub struct Receiver<T: Send + 'static> {
    chan: Arc<Channel<T>>,
}

impl<T: Send + 'static> Receiver<T> {
    pub fn new() -> Self {
        Receiver {
            chan: Arc::new(Channel::new()),
        }
    }
    pub fn sender(&self) -> Sender<T> {
        Sender {
            chan: self.chan.clone(),
        }
    }
    #[inline]
    pub fn try_recv(&self) -> io::Result<Option<T>> {
        self.chan.try_recv()
    }
}

impl<T: Send + 'static> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Receiver {
            chan: Arc::clone(&self.chan),
        }
    }
}

unsafe impl<T: Send + 'static> Send for Receiver<T> {}

pub fn new<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Channel::new());
    let tx = Sender { chan: chan.clone() };
    let rx = Receiver { chan: chan.clone() };
    (tx, rx)
}
