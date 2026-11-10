//! The lease table: at most one slot may write a given file at a time (SPEC section 4, ADR 0001).
//!
//! A lease is taken by a slot's first admitted write to a file and held until that slot's
//! session stops or ends. While it is held, a write to the file from any other slot is denied.
//! The table knows nothing about tools, clocks or sessions: the caller says which slot wants
//! which file, and which slot is letting go.
//!
//! Not here yet: the time limit, release commands, Defer, whole-tree leases for Bash, and the
//! Invalid standing that makes a slot re-read a file changed under it.

use std::collections::BTreeMap;

/// One slot's exclusive right to write one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub holder: String,
    /// What the holder said it was doing when it took the lease; empty when it said nothing.
    pub intent: String,
    /// Wall time the lease was taken, milliseconds since the Unix epoch. For the Deny reason only.
    pub taken_at: i64,
}

/// The run's leases, by file.
#[derive(Debug, Clone, Default)]
pub struct Leases {
    held: BTreeMap<String, Lease>,
}

impl Leases {
    /// `slot` wants to write `path`. Granted when the file is free or already its own, in which
    /// case the first lease stays as taken. Refused with the other slot's lease otherwise; the
    /// table is then unchanged.
    pub fn claim(&mut self, path: &str, slot: &str, intent: &str, now: i64) -> Result<(), &Lease> {
        let lease = self.held.entry(path.to_owned()).or_insert_with(|| Lease {
            holder: slot.to_owned(),
            intent: intent.to_owned(),
            taken_at: now,
        });
        if lease.holder == slot {
            Ok(())
        } else {
            Err(lease)
        }
    }

    /// Release every lease `slot` holds and return their files, in path order.
    pub fn release_all(&mut self, slot: &str) -> Vec<String> {
        let released: Vec<String> = self
            .held
            .iter()
            .filter(|(_, lease)| lease.holder == slot)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &released {
            self.held.remove(path);
        }
        released
    }

    /// The lease on `path`, if any.
    pub fn on(&self, path: &str) -> Option<&Lease> {
        self.held.get(path)
    }
}

/// The Deny a slot gets for writing `path` while `lease` is held: who holds it, their stated
/// intent, and for how long (SPEC section 4).
pub fn deny_reason(path: &str, lease: &Lease, now: i64) -> String {
    let intent = if lease.intent.is_empty() {
        "no stated intent".to_owned()
    } else {
        format!("intent: {:?}", lease.intent)
    };
    let held_s = (now - lease.taken_at).max(0) / 1000;
    format!(
        "{path} is leased by {} ({intent}), held for {}m {:02}s. Edits to it are refused until \
         the lease is released. Work on something else, or try again later.",
        lease.holder,
        held_s / 60,
        held_s % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_has_one_holder_until_that_slot_releases() {
        let mut leases = Leases::default();
        assert!(leases.claim("a.rs", "slot-1", "fix mean", 1_000).is_ok());
        assert!(leases.claim("a.rs", "slot-1", "again", 9_000).is_ok());
        assert_eq!(leases.on("a.rs").unwrap().taken_at, 1_000);
        let held = leases
            .claim("a.rs", "slot-2", "", 2_000)
            .unwrap_err()
            .clone();
        assert_eq!(
            (held.holder.as_str(), held.intent.as_str()),
            ("slot-1", "fix mean")
        );
        assert!(leases.claim("b.rs", "slot-2", "", 2_000).is_ok());
        assert!(leases.claim("c.rs", "slot-1", "", 2_000).is_ok());

        assert_eq!(leases.release_all("slot-1"), ["a.rs", "c.rs"]);
        assert!(leases.claim("a.rs", "slot-2", "", 3_000).is_ok());
        assert_eq!(leases.on("b.rs").unwrap().holder, "slot-2");
        assert!(leases.release_all("slot-1").is_empty());
    }

    #[test]
    fn the_deny_reason_names_the_holder_its_intent_and_the_lease_age() {
        let lease = Lease {
            holder: "slot-1".into(),
            intent: "fix mean".into(),
            taken_at: 10_000,
        };
        let reason = deny_reason("src/lib.rs", &lease, 135_500);
        assert!(reason.starts_with(
            "src/lib.rs is leased by slot-1 (intent: \"fix mean\"), held for 2m 05s."
        ));
        let silent = Lease {
            intent: String::new(),
            ..lease
        };
        assert!(deny_reason("src/lib.rs", &silent, 10_000)
            .contains("slot-1 (no stated intent), held for 0m 00s"));
    }
}
