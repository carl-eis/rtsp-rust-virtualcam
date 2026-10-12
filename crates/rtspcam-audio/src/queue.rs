use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::block::{BLOCK_DURATION, Block};

/// Above this much queued sound the oldest blocks are dropped (the Java bot uses 200 ms too).
pub const DEFAULT_LAG_LIMIT: Duration = Duration::from_millis(200);

/// A bounded queue of blocks between one producer (a capture callback) and one consumer (the
/// voice connection, or a playback callback).
///
/// When the queue holds more than the lag limit, [`push`](Self::push) drops the oldest blocks
/// and counts them: live sound should stay live rather than fall further and further behind.
///
/// A plain mutex: both sides hold it only to move a pointer in or out, so neither waits long.
#[derive(Debug)]
pub struct BlockQueue {
    blocks: Mutex<VecDeque<Block>>,
    capacity: usize,
    dropped: AtomicU64,
}

impl BlockQueue {
    /// A queue that holds at most `lag_limit` of sound (at least one block).
    pub fn new(lag_limit: Duration) -> Self {
        let capacity = (lag_limit.as_millis() / BLOCK_DURATION.as_millis()).max(1) as usize;
        Self {
            blocks: Mutex::new(VecDeque::with_capacity(capacity + 1)),
            capacity,
            dropped: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Block>> {
        self.blocks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a block at the end, dropping the oldest ones above the lag limit.
    pub fn push(&self, block: Block) {
        let mut blocks = self.lock();
        blocks.push_back(block);
        let excess = blocks.len().saturating_sub(self.capacity);
        if excess > 0 {
            blocks.drain(..excess);
            self.dropped.fetch_add(excess as u64, Ordering::Relaxed);
        }
    }

    /// Takes the oldest block, if any.
    pub fn pop(&self) -> Option<Block> {
        self.lock().pop_front()
    }

    /// Throws away everything queued (for example after unmuting, so stale sound isn't sent).
    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// How far behind live the queued sound is.
    pub fn lag(&self) -> Duration {
        BLOCK_DURATION * self.len() as u32
    }

    /// Blocks dropped so far because the consumer fell behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Default for BlockQueue {
    fn default() -> Self {
        Self::new(DEFAULT_LAG_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_LEN;

    fn numbered(n: f32) -> Block {
        Block::from_slice(&[n; BLOCK_LEN]).unwrap()
    }

    #[test]
    fn keeps_order() {
        let q = BlockQueue::default();
        q.push(numbered(1.0));
        q.push(numbered(2.0));
        assert_eq!(q.pop().unwrap()[0], 1.0);
        assert_eq!(q.pop().unwrap()[0], 2.0);
        assert!(q.pop().is_none());
    }

    #[test]
    fn drops_the_oldest_blocks_above_the_lag_limit() {
        let q = BlockQueue::new(Duration::from_millis(200));
        for i in 0..15 {
            q.push(numbered(i as f32));
        }
        assert_eq!(q.len(), 10);
        assert_eq!(q.lag(), Duration::from_millis(200));
        assert_eq!(q.dropped(), 5);
        // The newest sound is kept.
        assert_eq!(q.pop().unwrap()[0], 5.0);
    }

    #[test]
    fn a_tiny_limit_still_holds_one_block() {
        let q = BlockQueue::new(Duration::ZERO);
        q.push(numbered(1.0));
        q.push(numbered(2.0));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop().unwrap()[0], 2.0);
    }
}
