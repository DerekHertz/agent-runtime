//! The coherence rules of a run that takes leases: who may write a file now, and what a slot
//! learns when it reads one or is refused one (SPEC sections 4 and 5).
//!
//! This is the only place where a clock absorbs another (clock rule 2). There are two channels
//! here, and nothing else merges (ADR 0002):
//!
//! - **Read.** Reading a file absorbs its release clock, so the last writer's work up to its
//!   release happens-before everything the reader does next.
//! - **Deny.** A slot denied for a lease absorbs the holder's clock from when it took the lease,
//!   so the denied slot provably knew that write was under way.
//!
//! [`Leases`] and [`Files`] hold the two tables; this module only puts their rules in order.

use trace_store::Access;

use super::clocks::Clocks;
use super::files::{self, Files};
use super::leases::{self, Leases};

/// Why a write was denied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub path: String,
    pub reason: String,
    /// The slot holding the lease, when that is why. `None` when the slot must re-read first.
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Coherence {
    leases: Leases,
    files: Files,
}

impl Coherence {
    /// Apply the rules to `slot`'s call before its event is stamped. A Read makes the slot's
    /// view of the file current and absorbs the file's release clock. A write is denied while
    /// another slot holds the file, or while the file has changed since this slot last read it;
    /// otherwise it takes or keeps the lease. Anything else is allowed and changes nothing.
    pub fn admit(
        &mut self,
        clocks: &mut Clocks,
        slot: &str,
        access: &Access,
        intent: &str,
        now: i64,
    ) -> Option<Denied> {
        match access {
            Access::Other => None,
            Access::Read { path } => {
                if let Some(released) = self.files.read(path, slot) {
                    clocks.merge(slot, released);
                }
                None
            }
            Access::Write { path } => {
                if let Some(lease) = self.leases.on(path).filter(|l| l.holder != slot) {
                    clocks.merge(slot, &lease.taken_clock);
                    return Some(Denied {
                        path: path.clone(),
                        reason: leases::deny_reason(path, lease, now),
                        owner: Some(lease.holder.clone()),
                    });
                }
                if self.files.is_stale_for(path, slot) {
                    return Some(Denied {
                        path: path.clone(),
                        reason: files::reread_reason(path),
                        owner: None,
                    });
                }
                // The lease's clock is the stamp this write is about to get.
                let stamp = clocks.next_stamp(slot)?;
                let claimed = self.leases.claim(path, slot, intent, now, &stamp);
                debug_assert!(
                    claimed.is_ok(),
                    "no other slot holds the file: checked above"
                );
                None
            }
        }
    }

    /// `slot`'s turn or session is over, and its closing event is stamped: release every lease
    /// it holds. Each released file takes the slot's clock as its release clock, and every other
    /// slot's view of it goes stale.
    pub fn release(&mut self, clocks: &Clocks, slot: &str) {
        let Some(clock) = clocks.get(slot) else {
            return;
        };
        let slots = clocks.slots();
        for path in self.leases.release_all(slot) {
            self.files.release(&path, slot, clock, &slots);
        }
    }

    /// The slot holding the lease on `path`, if any.
    pub fn lease_holder(&self, path: &str) -> Option<&str> {
        self.leases.on(path).map(|lease| lease.holder.as_str())
    }

    /// Whether `slot` must re-read `path` before writing it.
    pub fn is_stale_for(&self, path: &str, slot: &str) -> bool {
        self.files.is_stale_for(path, slot)
    }
}
