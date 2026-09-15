//! Pure bookkeeping for a ring of GPU timestamp-query readback slots.
//!
//! A GPU timestamp write plus its async `map_async` readback cannot be read
//! back the same frame it was written -- the callback fires whenever the
//! driver gets around to it, some frames later. A ring of slots lets frame
//! N+1 write into a fresh slot while frame N's slot is still being mapped, so
//! a caller never blocks on the GPU to get a reading.
//!
//! This module is only the state machine that decides which slot is safe to
//! write and which is safe to read -- no wgpu calls -- so it is unit-tested
//! without a device. The GPU-owning half (the actual `QuerySet` and readback
//! buffers) lives with whoever uses it (`cubara-app`'s `bench.rs`), built on
//! top of this.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    /// No outstanding GPU work; safe to write new queries into.
    Idle,
    /// Queries written and submitted, `map_async` started. Not safe to read
    /// -- `get_mapped_range` on a buffer whose map hasn't completed panics.
    Mapping,
    /// `map_async`'s callback has fired; safe to read once, then the slot
    /// goes back to `Idle`.
    Ready,
}

/// Round-robins `depth` slots of a timestamp query/readback buffer.
pub struct TimestampRing {
    slots: Vec<Slot>,
}

impl TimestampRing {
    pub fn new(depth: usize) -> Self {
        assert!(depth > 0, "a ring needs at least one slot");
        Self {
            slots: vec![Slot::Idle; depth],
        }
    }

    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    /// Whether `slot` has no outstanding GPU work and can be written this frame.
    pub fn can_write(&self, slot: usize) -> bool {
        self.slots[slot] == Slot::Idle
    }

    /// Call right after submitting `slot`'s query writes and starting its
    /// `map_async`. Panics if `slot` already has outstanding work -- that
    /// would mean a second `map_async` on a buffer that is already mapped.
    pub fn begin_mapping(&mut self, slot: usize) {
        assert!(
            self.can_write(slot),
            "slot {slot} already has outstanding GPU work"
        );
        self.slots[slot] = Slot::Mapping;
    }

    /// Call from `map_async`'s callback once it fires successfully.
    pub fn mark_ready(&mut self, slot: usize) {
        self.slots[slot] = Slot::Ready;
    }

    /// If `slot` is ready, mark it `Idle` again (so it can be reused) and
    /// return `true` -- the caller may now call `get_mapped_range` on it.
    /// Returns `false` (without changing state) if the slot is `Idle`
    /// (nothing was ever written) or `Mapping` (the callback has not fired
    /// yet) -- reading in either case would be wrong: `Idle` has no data,
    /// `Mapping` is not actually mapped yet.
    pub fn take_ready(&mut self, slot: usize) -> bool {
        if self.slots[slot] == Slot::Ready {
            self.slots[slot] = Slot::Idle;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_ring_has_nothing_ready_to_read() {
        let mut ring = TimestampRing::new(3);
        for slot in 0..3 {
            assert!(!ring.take_ready(slot));
        }
    }

    #[test]
    fn a_slot_is_readable_only_after_its_callback_marks_it_ready() {
        let mut ring = TimestampRing::new(3);
        assert!(ring.can_write(0));
        ring.begin_mapping(0);
        assert!(!ring.can_write(0));
        // The callback hasn't fired yet: reading now would be reading an
        // unmapped buffer.
        assert!(!ring.take_ready(0));
        ring.mark_ready(0);
        assert!(ring.take_ready(0));
        // Taken once; the slot goes back to idle rather than being readable twice.
        assert!(!ring.take_ready(0));
        assert!(ring.can_write(0));
    }

    #[test]
    fn slots_cycle_independently() {
        let mut ring = TimestampRing::new(3);
        ring.begin_mapping(0);
        ring.begin_mapping(1);
        // Slot 2 was never written, so it is still writable and not readable.
        assert!(ring.can_write(2));
        assert!(!ring.take_ready(2));
        ring.mark_ready(1);
        // Only slot 1 is ready -- slot 0's callback has not fired.
        assert!(!ring.take_ready(0));
        assert!(ring.take_ready(1));
    }

    #[test]
    #[should_panic]
    fn writing_a_slot_with_outstanding_work_is_a_bug() {
        let mut ring = TimestampRing::new(2);
        ring.begin_mapping(0);
        // Still `Mapping` -- calling `begin_mapping` again without a
        // `take_ready` in between would start a second `map_async` on a
        // buffer that's already mapped, which is the bug this type exists to
        // make impossible to write by accident.
        ring.begin_mapping(0);
    }
}
