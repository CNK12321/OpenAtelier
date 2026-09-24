//! Single-producer, single-consumer sample ring.
//!
//! The audio callback is the consumer and must never block or allocate, so the only
//! synchronization is two atomics. The producer owns `write`, the consumer owns `read`.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Ring {
    buffer: UnsafeCell<Box<[f32]>>,
    capacity: usize,
    read: AtomicUsize,
    write: AtomicUsize,
}

// Safety: the producer only touches `write` and the slots between `write` and `read`;
// the consumer only touches `read` and the slots between `read` and `write`.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    /// Holds `capacity - 1` samples (one slot distinguishes full from empty).
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 2);
        Ring {
            buffer: UnsafeCell::new(vec![0.0; capacity].into_boxed_slice()),
            capacity,
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    /// Samples waiting to be played.
    pub fn available(&self) -> usize {
        let (r, w) = (self.read.load(Ordering::Acquire), self.write.load(Ordering::Acquire));
        if w >= r { w - r } else { self.capacity() - r + w }
    }

    pub fn free(&self) -> usize {
        self.capacity() - 1 - self.available()
    }

    /// Producer side: writes as much of `src` as fits, returning how much was written.
    pub fn push(&self, src: &[f32]) -> usize {
        let capacity = self.capacity();
        let write = self.write.load(Ordering::Relaxed);
        let n = src.len().min(self.free());
        // Safety: these slots are between `write` and `read`, which only we write to.
        let buffer = unsafe { &mut *self.buffer.get() };
        for (i, sample) in src[..n].iter().enumerate() {
            buffer[(write + i) % capacity] = *sample;
        }
        self.write.store((write + n) % capacity, Ordering::Release);
        n
    }

    /// Consumer side: fills `dst` and returns how many samples were real (the rest is
    /// silence — an underrun).
    pub fn pop(&self, dst: &mut [f32]) -> usize {
        let capacity = self.capacity();
        let read = self.read.load(Ordering::Relaxed);
        let n = dst.len().min(self.available());
        // Safety: these slots are between `read` and `write`, which only we read from.
        let buffer = unsafe { &*self.buffer.get() };
        for (i, slot) in dst[..n].iter_mut().enumerate() {
            *slot = buffer[(read + i) % capacity];
        }
        dst[n..].fill(0.0);
        self.read.store((read + n) % capacity, Ordering::Release);
        n
    }

    /// Consumer side: drops everything buffered (used when seeking).
    pub fn clear(&self) {
        self.read.store(self.write.load(Ordering::Acquire), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_capacity_wraps_correctly() {
        // Capacities that aren't powers of two must wrap just as well.
        let ring = Ring::new(7);
        for round in 0..20 {
            let block: Vec<f32> = (0..4).map(|i| (round * 4 + i) as f32).collect();
            assert_eq!(ring.push(&block), 4, "round {round}");
            let mut got = vec![0.0; 4];
            assert_eq!(ring.pop(&mut got), 4);
            assert_eq!(got, block);
            assert_eq!(ring.available(), 0);
        }
    }

    #[test]
    fn push_pop_and_wrap() {
        let ring = Ring::new(8);
        assert_eq!((ring.available(), ring.free()), (0, 7));
        assert_eq!(ring.push(&[1.0, 2.0, 3.0]), 3);
        let mut out = [0.0; 2];
        assert_eq!(ring.pop(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        let mut rest = [0.0; 1];
        assert_eq!(ring.pop(&mut rest), 1);
        assert_eq!((rest[0], ring.available()), (3.0, 0));
        // Wrap around the end of the buffer several times.
        for round in 0..10 {
            let block: Vec<f32> = (0..5).map(|i| (round * 5 + i) as f32).collect();
            assert_eq!(ring.push(&block), 5);
            let mut got = vec![0.0; 5];
            assert_eq!(ring.pop(&mut got), 5);
            assert_eq!(got, block);
        }
    }

    #[test]
    fn full_ring_refuses_and_underrun_is_silence() {
        let ring = Ring::new(4);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]), 3, "one slot is reserved");
        let mut out = [9.0; 5];
        assert_eq!(ring.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0, 0.0, 0.0], "underrun pads with silence");
    }

    #[test]
    fn clear_drops_buffered_audio() {
        let ring = Ring::new(16);
        ring.push(&[1.0; 10]);
        ring.clear();
        assert_eq!(ring.available(), 0);
        let mut out = [7.0; 4];
        assert_eq!(ring.pop(&mut out), 0);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn producer_and_consumer_on_two_threads() {
        let ring = std::sync::Arc::new(Ring::new(1024));
        let producer = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                let mut written = 0usize;
                while written < 100_000 {
                    let block: Vec<f32> = (0..64).map(|i| (written + i) as f32).collect();
                    let n = ring.push(&block);
                    written += n;
                    if n == 0 {
                        std::thread::yield_now();
                    }
                }
            })
        };
        let mut expected = 0.0f32;
        let mut got = vec![0.0; 64];
        while expected < 100_000.0 {
            let n = ring.pop(&mut got);
            for sample in &got[..n] {
                assert_eq!(*sample, expected, "samples must arrive in order without gaps");
                expected += 1.0;
            }
            if n == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}
