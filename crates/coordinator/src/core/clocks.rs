//! Per-slot clocks (SPEC section 5, clock rules 1 and 2).
//!
//! Maps each slot name to its position in every clock and holds one `clock::Clock` per slot. The
//! coordinator holds all of them because shims are stateless.

use std::collections::HashMap;

use clock::Clock;

#[derive(Debug, Clone)]
pub struct Clocks {
    index: HashMap<String, usize>,
    clocks: Vec<Clock>,
}

impl Clocks {
    /// One zero clock per slot, each with one entry per slot. Callers validate the slot list
    /// first (see `RunConfig::validate`): names are unique, so a slot's position is its index.
    pub fn new(slots: &[String]) -> Self {
        let index = slots
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i))
            .collect();
        let clocks = vec![Clock::new(slots.len()); slots.len()];
        Self { index, clocks }
    }

    /// The slot's current clock, or `None` for a slot not in the run.
    pub fn get(&self, slot: &str) -> Option<&Clock> {
        self.index.get(slot).map(|&i| &self.clocks[i])
    }

    /// Clock rule 1: increment the slot's own entry and return the stamp (its full clock).
    /// `None` for a slot not in the run, in which case nothing changes.
    /// Clock rule 2: `slot` learns everything `other` records, through a knowledge channel. The
    /// caller names the channel; nothing else may merge (ADR 0002).
    pub fn merge(&mut self, slot: &str, other: &Clock) {
        if let Some(&i) = self.index.get(slot) {
            self.clocks[i].merge(other);
        }
    }

    /// The run's slots, in clock order.
    pub fn slots(&self) -> Vec<String> {
        let mut slots: Vec<(&String, &usize)> = self.index.iter().collect();
        slots.sort_by_key(|(_, &i)| i);
        slots.into_iter().map(|(name, _)| name.clone()).collect()
    }

    /// The stamp `slot`'s next event will get if nothing merges first.
    pub fn next_stamp(&self, slot: &str) -> Option<Clock> {
        let &i = self.index.get(slot)?;
        let mut next = self.clocks[i].clone();
        next.tick(i);
        Some(next)
    }

    pub fn tick(&mut self, slot: &str) -> Option<&Clock> {
        let &i = self.index.get(slot)?;
        self.clocks[i].tick(i);
        Some(&self.clocks[i])
    }
}
