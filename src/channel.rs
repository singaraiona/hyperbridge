use crate::util::{Backoff, CachePadded};
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use core::sync::atomic::{fence, AtomicPtr, AtomicUsize};
use std::sync::Arc;
use std::{fmt, io};

// Slot states
const WROTE: usize = 1;
const READ: usize = 1 << 1;
const DESTROY: usize = 1 << 2;
// Index offset inside each block
const INDEX_SHIFT: usize = 48;
// In tail indicates that channel was closed
const CLOSED_FLAG: usize = 1;
// In head indicates that head and tail are in separate blocks
const CROSSED_FLAG: usize = 1;
const FLAGS: usize = CLOSED_FLAG | CROSSED_FLAG;
const INDEX_MASK: usize = (usize::MAX << INDEX_SHIFT) & !FLAGS;
const BLOCK_MASK: usize = !(INDEX_MASK | FLAGS);
// Each block capacity
const BLOCK_ROUND: usize = 64;
const BLOCK_SIZE: usize = BLOCK_ROUND - 1;

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
    fn new() -> *mut Block<T> {
        let block: Block<T> = unsafe { MaybeUninit::zeroed().assume_init() };
        Box::into_raw(Box::new(block))
    }

    /// Blocks current thread waiting the next block is set
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

    /// Sets up a new next block
    fn set_next(&self) -> *mut Block<T> {
        debug_assert!(self.next.load(Acquire).is_null());
        let next = Self::new();
        self.next.store(next, Release);
        next
    }

    /// Returns current next block (if any)
    fn get_next(&self) -> Option<*mut Block<T>> {
        let next = self.next.load(Acquire);
        if next.is_null() {
            None
        } else {
            Some(next)
        }
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

/// A helper struct holds pointer to a with a slot offset and a flags
struct Position<T> {
    block: *mut Block<T>,
    index: usize,
    flags: usize,
}

impl<T> fmt::Debug for Position<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Position")
            .field("block", &self.block)
            .field("index", &self.index)
            .finish()
    }
}

impl<T> Position<T> {
    /// Read Position packed to a usize
    #[inline]
    fn unpack(val: usize) -> Self {
        let block = (val & BLOCK_MASK) as *mut Block<T>;
        let index = val >> INDEX_SHIFT;
        let flags = val & FLAGS;
        Position {
            block,
            index,
            flags,
        }
    }

    // Create Position from parts instead of plain number
    #[inline]
    fn from_parts(block: *mut Block<T>, index: usize, flags: usize) -> Self {
        Position {
            block,
            index,
            flags,
        }
    }

    /// Write Position to a usize
    #[inline]
    fn pack(&self) -> usize {
        self.block as usize | (self.index << INDEX_SHIFT) | self.flags
    }

    /// Move Position to a next Slot
    #[inline]
    fn increment(&self, n: u16) -> Self {
        Position {
            block: self.block,
            index: (self.index as u16).wrapping_add(n) as usize,
            flags: self.flags,
        }
    }

    // Coerse index to an offset 0..BLOCK_SIZE
    #[inline]
    fn offset(&self) -> usize {
        self.index & BLOCK_SIZE
    }

    /// A Slot this Position points to
    #[inline]
    fn slot(&self) -> &Slot<T> {
        unsafe { &*(*self.block).slots.get_unchecked(self.offset()) }
    }

    // Positions are in a different blocks
    #[inline]
    fn crossed(&self, other: &Self) -> bool {
        self.index / BLOCK_ROUND != other.index / BLOCK_ROUND
    }
}

impl<T> PartialEq for Position<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.block as usize | (self.index << INDEX_SHIFT)
            == other.block as usize | (other.index << INDEX_SHIFT)
    }
}

/// An atomic position
#[repr(transparent)]
struct Cursor<T> {
    inner: AtomicUsize,
    _phantom: PhantomData<T>,
}

impl<T> Cursor<T> {
    /// Create Cursor from Position
    #[inline]
    fn from(position: Position<T>) -> Self {
        debug_assert!(position.block as usize & !BLOCK_MASK == 0);
        Cursor {
            inner: AtomicUsize::new(position.block as usize | position.index << INDEX_SHIFT),
            _phantom: PhantomData,
        }
    }
}

impl<T> Deref for Cursor<T> {
    type Target = AtomicUsize;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Unbounded channel
struct Channel<T> {
    tail: CachePadded<Cursor<Block<T>>>,
    head: CachePadded<Cursor<Block<T>>>,
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        // read all unread items to drop correctly
        while let Ok(Some(_)) = self.try_recv() {}

        let head_packed = self.head.load(Acquire);
        let head: Position<T> = Position::unpack(head_packed);

        // noone is using the block, now it is safe to destroy it.
        unsafe { drop(Box::from_raw(head.block)) };
    }
}

impl<T> Channel<T> {
    /// Creates new unbounded channel
    fn new() -> Channel<T> {
        let block = Block::<T>::new();
        Channel {
            tail: CachePadded::new(Cursor::from(Position::unpack(block as usize))),
            head: CachePadded::new(Cursor::from(Position::unpack(block as usize))),
        }
    }

    /// Try to send a message to a channel.
    /// Can not fail but when channel is closed
    #[inline]
    fn send(&self, msg: T) -> io::Result<()> {
        let backoff = Backoff::new();
        let mut tail_packed = self.tail.load(Acquire);

        loop {
            let tail: Position<T> = Position::unpack(tail_packed);
            let offset = tail.offset();

            // channel is closed
            if tail.flags & CLOSED_FLAG == CLOSED_FLAG {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel is closed",
                ));
            }

            // wait next block
            if offset == BLOCK_SIZE {
                backoff.snooze();
                tail_packed = self.tail.load(Acquire);
                continue;
            }

            // try to move tail forward
            match self.tail.compare_exchange_weak(
                tail_packed,
                tail.increment(1).pack(),
                SeqCst,
                Relaxed,
            ) {
                Ok(_) => {
                    let slot = tail.slot();

                    // End of block, need to setup new one
                    if offset + 1 == BLOCK_SIZE {
                        let next_block = unsafe { (*tail.block).set_next() };
                        let new_tail: Position<T> =
                            Position::from_parts(next_block, tail.index, 0).increment(2);
                        self.tail.store(new_tail.pack(), Release);
                    }

                    unsafe { slot.message.get().write(MaybeUninit::new(msg)) };
                    slot.state.fetch_or(WROTE, Release);
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

    /// Try to receive message from a channel.
    /// Can fail or return uncompleted.
    #[inline]
    fn try_recv(&self) -> io::Result<Option<T>> {
        let backoff = Backoff::new();
        let mut head_packed = self.head.load(Acquire);

        loop {
            let mut head: Position<T> = Position::unpack(head_packed);
            let offset = head.offset();

            // wait next block
            if offset == BLOCK_SIZE {
                backoff.snooze();
                head_packed = self.head.load(Acquire);
                continue;
            }

            // head and tail are (possibly) in the same block
            if head.flags & CROSSED_FLAG == 0 {
                fence(SeqCst);
                let tail_packed = self.tail.load(Relaxed);
                let tail: Position<T> = Position::unpack(tail_packed);

                // channel is empty
                if head == tail {
                    // channel is closed
                    if tail.flags & CLOSED_FLAG == CLOSED_FLAG {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "channel is closed",
                        ));
                    }
                    return Ok(None);
                }

                // head and tail are crossed, mark head
                if head.crossed(&tail) {
                    head.flags |= CROSSED_FLAG;
                }
            }

            // try to move head forward
            match self.head.compare_exchange_weak(
                head_packed,
                head.increment(1).pack(),
                SeqCst,
                Relaxed,
            ) {
                Ok(_) => unsafe {
                    // last slot in a block
                    if offset + 1 == BLOCK_SIZE {
                        let next_block = (*head.block).wait_next();
                        // if next block is not the last one, mark head
                        let flags = if (*next_block).get_next().is_some() {
                            CROSSED_FLAG
                        } else {
                            0
                        };

                        let new_head: Position<T> =
                            Position::from_parts(next_block, head.index, flags).increment(2);
                        self.head.store(new_head.pack(), Release);
                    }

                    let slot = head.slot();

                    // wait until write operation completes
                    while slot.state.load(Acquire) & WROTE == 0 {
                        backoff.spin();
                    }

                    let msg = slot.message.get().read().assume_init();

                    // this is the last block, so start destroying it
                    if offset + 1 == BLOCK_SIZE {
                        Block::destroy(head.block, 0);
                    }
                    // someone started block destroy
                    else if slot.state.fetch_or(READ, AcqRel) & DESTROY != 0 {
                        Block::destroy(head.block, offset + 1);
                    }

                    return Ok(Some(msg));
                },

                Err(h) => {
                    head_packed = h;
                    backoff.spin();
                }
            }
        }
    }

    /// Check if there are no mesages in a channel
    #[inline]
    fn is_empty(&self) -> bool {
        let head = self.head.load(SeqCst);
        let tail = self.tail.load(SeqCst);
        head == tail
    }

    // Closes a channel
    #[inline]
    fn close(&self) {
        self.tail.fetch_or(CLOSED_FLAG, AcqRel);
    }
}

impl<T> fmt::Debug for Channel<T> {
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

impl<T> fmt::Debug for Sender<T> {
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
