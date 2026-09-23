//! A byte stream held by absolute offset, keeping only its most recent part.
//!
//! Both ends of the link keep one of these per output stream: the agent holds
//! what the host has not received yet, the host holds what its reader has not
//! consumed yet. Offsets never restart, so "give me everything from byte N" has
//! one meaning on both sides even after the oldest bytes are gone.

use std::collections::VecDeque;

#[derive(Debug)]
pub struct Ring {
    bytes: VecDeque<u8>,
    /// Offset of `bytes[0]` in the stream.
    start: u64,
    capacity: usize,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            start: 0,
            capacity: capacity.max(1),
        }
    }

    /// Offset one past the last byte ever pushed.
    pub fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    /// Offset of the oldest byte still held.
    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Appends `data`, dropping the oldest bytes past capacity.
    pub fn push(&mut self, data: &[u8]) {
        if data.len() >= self.capacity {
            let skipped = data.len() - self.capacity;
            self.start += self.bytes.len() as u64 + skipped as u64;
            self.bytes.clear();
            self.bytes.extend(&data[skipped..]);
            return;
        }
        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.start += overflow as u64;
        }
        self.bytes.extend(data);
    }

    /// Up to `max` bytes starting at `offset`, clamped to what is still held.
    /// Returns the offset the bytes actually start at: later than `offset`
    /// when the bytes in between were dropped.
    pub fn read_from(&self, offset: u64, max: usize) -> (u64, Vec<u8>) {
        let from = offset.clamp(self.start, self.end());
        let skip = (from - self.start) as usize;
        let take = (self.bytes.len() - skip).min(max);
        let (front, back) = self.bytes.as_slices();
        let mut out = Vec::with_capacity(take);
        if skip < front.len() {
            let head = &front[skip..(skip + take).min(front.len())];
            out.extend_from_slice(head);
            if out.len() < take {
                out.extend_from_slice(&back[..take - out.len()]);
            }
        } else {
            let at = skip - front.len();
            out.extend_from_slice(&back[at..at + take]);
        }
        (from, out)
    }

    /// Drops everything before `offset`: the consumer has it.
    pub fn consume_to(&mut self, offset: u64) {
        let offset = offset.min(self.end());
        if offset > self.start {
            let count = (offset - self.start) as usize;
            self.bytes.drain(..count);
            self.start = offset;
        }
    }

    /// Moves the stream to `offset` without holding the bytes in between,
    /// which a reader that learns of a gap does before continuing past it.
    pub fn skip_to(&mut self, offset: u64) {
        if offset > self.end() {
            self.bytes.clear();
            self.start = offset;
        } else {
            self.consume_to(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_keep_counting_after_the_oldest_bytes_are_dropped() {
        let mut ring = Ring::new(8);
        ring.push(b"abcdef");
        ring.push(b"ghij");
        assert_eq!((ring.start(), ring.end()), (2, 10));
        assert_eq!(ring.read_from(0, 100), (2, b"cdefghij".to_vec()));
        assert_eq!(ring.read_from(7, 2), (7, b"hi".to_vec()));
        assert_eq!(ring.read_from(10, 5), (10, Vec::new()));
        ring.push(b"0123456789AB");
        assert_eq!((ring.start(), ring.end()), (14, 22));
        assert_eq!(ring.read_from(0, 3), (14, b"456".to_vec()));
    }

    #[test]
    fn reads_across_the_deque_seam_are_contiguous() {
        let mut ring = Ring::new(6);
        for chunk in [&b"abcd"[..], b"ef", b"gh", b"ij"] {
            ring.push(chunk);
        }
        assert_eq!(ring.read_from(4, 6), (4, b"efghij".to_vec()));
        assert_eq!(ring.read_from(5, 3), (5, b"fgh".to_vec()));
    }

    #[test]
    fn consuming_and_skipping_move_the_start() {
        let mut ring = Ring::new(16);
        ring.push(b"hello world");
        ring.consume_to(6);
        assert_eq!(ring.read_from(0, 100), (6, b"world".to_vec()));
        ring.skip_to(20);
        assert_eq!((ring.start(), ring.end()), (20, 20));
        ring.push(b"!");
        assert_eq!(ring.read_from(20, 10), (20, b"!".to_vec()));
    }
}
