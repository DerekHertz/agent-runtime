//! Vector clock: one counter per slot in a run (SPEC section 5, GLOSSARY "Clock").
//!
//! Pure: no I/O, no wall time. Slots are positions `0..n`; mapping slot names to positions is the
//! coordinator's job.
//!
//! Contract:
//! - A clock created with `Clock::new(n)` has `n` entries, all zero.
//! - Missing entries are treated as zero everywhere, so clocks of different lengths compare and
//!   merge as if the shorter one were zero-padded. `merge` grows `self` to the longer length.
//! - `to_bytes` is the trace's `vclock` column: each entry as a little-endian u64, in slot order
//!   (8 bytes per slot). `from_bytes` is its inverse and rejects lengths not divisible by 8.

/// How two clocks relate in the happens-before partial order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// `self` happens-before `other`: every entry <=, at least one <.
    Before,
    /// `other` happens-before `self`.
    After,
    /// Identical entries.
    Equal,
    /// Neither happens-before the other.
    Concurrent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clock {
    entries: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub len: usize,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "clock bytes length {} is not a multiple of 8", self.len)
    }
}

impl std::error::Error for DecodeError {}

impl Clock {
    /// A zero clock with one entry per slot.
    pub fn new(slots: usize) -> Self {
        Self {
            entries: vec![0; slots],
        }
    }

    pub fn from_entries(entries: Vec<u64>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[u64] {
        &self.entries
    }

    /// Increment `slot`'s own entry (clock rule 1). Panics if `slot` is out of range.
    pub fn tick(&mut self, slot: usize) {
        self.entries[slot] += 1;
    }

    /// Element-wise max with `other` (clock rule 2, used only by knowledge channels).
    pub fn merge(&mut self, other: &Clock) {
        if other.entries.len() > self.entries.len() {
            self.entries.resize(other.entries.len(), 0);
        }
        for (mine, theirs) in self.entries.iter_mut().zip(&other.entries) {
            *mine = (*mine).max(*theirs);
        }
    }

    /// Four-way comparison in the happens-before partial order.
    pub fn compare(&self, other: &Clock) -> Order {
        let (mut some_less, mut some_greater) = (false, false);
        for i in 0..self.entries.len().max(other.entries.len()) {
            let mine = self.entries.get(i).copied().unwrap_or(0);
            let theirs = other.entries.get(i).copied().unwrap_or(0);
            some_less |= mine < theirs;
            some_greater |= mine > theirs;
        }
        match (some_less, some_greater) {
            (false, false) => Order::Equal,
            (true, false) => Order::Before,
            (false, true) => Order::After,
            (true, true) => Order::Concurrent,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.entries.iter().flat_map(|e| e.to_le_bytes()).collect()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if !bytes.len().is_multiple_of(8) {
            return Err(DecodeError { len: bytes.len() });
        }
        let entries = bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("chunk is 8 bytes")))
            .collect();
        Ok(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(entries: &[u64]) -> Clock {
        Clock::from_entries(entries.to_vec())
    }

    #[test]
    fn new_is_all_zero() {
        assert_eq!(Clock::new(3).entries(), &[0, 0, 0]);
        assert!(Clock::new(0).entries().is_empty());
    }

    #[test]
    fn tick_increments_only_own_slot() {
        let mut a = Clock::new(3);
        a.tick(1);
        a.tick(1);
        assert_eq!(a.entries(), &[0, 2, 0]);
    }

    #[test]
    #[should_panic]
    fn tick_out_of_range_panics() {
        Clock::new(2).tick(2);
    }

    #[test]
    fn merge_is_elementwise_max_and_grows() {
        let mut a = c(&[3, 0]);
        a.merge(&c(&[1, 4, 2]));
        assert_eq!(a.entries(), &[3, 4, 2]);
        let mut b = c(&[1, 4, 2]);
        b.merge(&c(&[3]));
        assert_eq!(b.entries(), &[3, 4, 2]);
    }

    #[test]
    fn compare_four_ways() {
        assert_eq!(c(&[1, 1]).compare(&c(&[1, 1])), Order::Equal);
        assert_eq!(c(&[1, 0]).compare(&c(&[1, 1])), Order::Before);
        assert_eq!(c(&[2, 1]).compare(&c(&[1, 1])), Order::After);
        assert_eq!(c(&[2, 0]).compare(&c(&[1, 1])), Order::Concurrent);
    }

    #[test]
    fn compare_zero_pads_shorter() {
        assert_eq!(c(&[1]).compare(&c(&[1, 0, 0])), Order::Equal);
        assert_eq!(c(&[1]).compare(&c(&[1, 0, 2])), Order::Before);
        assert_eq!(c(&[1, 0, 2]).compare(&c(&[1])), Order::After);
        assert_eq!(c(&[2]).compare(&c(&[1, 0, 2])), Order::Concurrent);
    }

    #[test]
    fn bytes_are_little_endian_per_slot() {
        assert_eq!(
            c(&[1, 0x0102]).to_bytes(),
            [1, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]
        );
        assert!(Clock::new(0).to_bytes().is_empty());
    }

    #[test]
    fn from_bytes_rejects_bad_lengths() {
        for len in [1, 7, 9, 15] {
            assert_eq!(Clock::from_bytes(&vec![0; len]), Err(DecodeError { len }));
        }
        assert_eq!(Clock::from_bytes(&[]), Ok(Clock::new(0)));
    }
}
