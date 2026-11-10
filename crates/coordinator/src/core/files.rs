//! What each slot knows of each file: the file's release clock, and the slots whose view of it
//! is stale (SPEC sections 4 and 5, ADR 0002).
//!
//! When a slot releases a file it wrote, the file's release clock becomes that slot's clock, and
//! every other slot's view of the file goes stale (the spec's Invalid standing). A stale slot
//! may not write the file. Reading it makes the view current again, and the reader takes the
//! release clock into its own: that is the Read knowledge channel, and it is what orders the
//! writer's work before everything the reader does next.
//!
//! A file no slot has released has no entry: nobody's view of it is stale, and reading it
//! teaches nothing. This differs from the spec, where a file a slot never read starts Invalid;
//! Claude Code's own Edit already requires a prior Read.

use std::collections::{HashMap, HashSet};

use clock::Clock;

#[derive(Debug, Clone)]
struct Released {
    clock: Clock,
    stale_for: HashSet<String>,
}

/// The run's released files.
#[derive(Debug, Clone, Default)]
pub struct Files {
    released: HashMap<String, Released>,
}

impl Files {
    /// `by` released `path` with changes, at `clock`. Every slot of `slots` but `by` now has a
    /// stale view of it.
    pub fn release(&mut self, path: &str, by: &str, clock: &Clock, slots: &[String]) {
        let stale_for = slots.iter().filter(|s| *s != by).cloned().collect();
        let released = Released {
            clock: clock.clone(),
            stale_for,
        };
        self.released.insert(path.to_owned(), released);
    }

    /// `slot` reads `path`: its view is current again. Returns the release clock it absorbs, or
    /// `None` when the file was never released.
    pub fn read(&mut self, path: &str, slot: &str) -> Option<&Clock> {
        let released = self.released.get_mut(path)?;
        released.stale_for.remove(slot);
        Some(&released.clock)
    }

    /// Whether `path` was released with changes since `slot` last read it.
    pub fn is_stale_for(&self, path: &str, slot: &str) -> bool {
        self.released
            .get(path)
            .is_some_and(|released| released.stale_for.contains(slot))
    }
}

/// The Deny a slot gets for writing a file that changed since it last read it.
pub fn reread_reason(path: &str) -> String {
    format!(
        "{path} changed since you last read it: another agent released it with changes. \
         Re-read first: use the Read tool on it, then make your edit again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_makes_every_other_slots_view_stale_until_that_slot_reads() {
        let slots: Vec<String> = ["slot-1", "slot-2", "slot-3"].map(String::from).to_vec();
        let mut files = Files::default();
        assert!(!files.is_stale_for("a.rs", "slot-2"));
        assert_eq!(files.read("a.rs", "slot-2"), None);

        let clock = Clock::from_entries(vec![4, 0, 0]);
        files.release("a.rs", "slot-1", &clock, &slots);
        assert!(!files.is_stale_for("a.rs", "slot-1"));
        assert!(files.is_stale_for("a.rs", "slot-2") && files.is_stale_for("a.rs", "slot-3"));
        assert!(!files.is_stale_for("b.rs", "slot-2"));

        assert_eq!(files.read("a.rs", "slot-2"), Some(&clock));
        assert!(!files.is_stale_for("a.rs", "slot-2") && files.is_stale_for("a.rs", "slot-3"));

        // A later release replaces the clock and makes the earlier reader stale again.
        let later = Clock::from_entries(vec![4, 9, 0]);
        files.release("a.rs", "slot-2", &later, &slots);
        assert!(files.is_stale_for("a.rs", "slot-1") && !files.is_stale_for("a.rs", "slot-2"));
        assert_eq!(files.read("a.rs", "slot-1"), Some(&later));
    }
}
