//! Property tests: `compare` is a partial order, `merge` is its least upper bound.

use clock::{Clock, Order};
use proptest::prelude::*;

/// Small slot counts and small values, so Equal and Before occur as often as Concurrent.
fn clock() -> impl Strategy<Value = Clock> {
    prop::collection::vec(0u64..4, 0..6).prop_map(Clock::from_entries)
}

fn padded(c: &Clock, len: usize) -> Vec<u64> {
    let mut v = c.entries().to_vec();
    v.resize(len, 0);
    v
}

fn mirror(o: Order) -> Order {
    match o {
        Order::Before => Order::After,
        Order::After => Order::Before,
        other => other,
    }
}

/// `a` dominates `b`: a is After or Equal to b.
fn dominates(a: &Clock, b: &Clock) -> bool {
    matches!(a.compare(b), Order::After | Order::Equal)
}

fn merged(a: &Clock, b: &Clock) -> Clock {
    let mut m = a.clone();
    m.merge(b);
    m
}

proptest! {
    #[test]
    fn compare_is_reflexive(a in clock()) {
        prop_assert_eq!(a.compare(&a), Order::Equal);
    }

    #[test]
    fn compare_is_dual(a in clock(), b in clock()) {
        prop_assert_eq!(a.compare(&b), mirror(b.compare(&a)));
    }

    #[test]
    fn compare_is_transitive(a in clock(), b in clock(), c in clock()) {
        if a.compare(&b) == Order::Before && b.compare(&c) == Order::Before {
            prop_assert_eq!(a.compare(&c), Order::Before);
        }
    }

    #[test]
    fn equal_iff_zero_padded_entries_equal(a in clock(), b in clock()) {
        let len = a.entries().len().max(b.entries().len());
        prop_assert_eq!(a.compare(&b) == Order::Equal, padded(&a, len) == padded(&b, len));
    }

    #[test]
    fn merge_is_an_upper_bound(a in clock(), b in clock()) {
        let m = merged(&a, &b);
        prop_assert!(dominates(&m, &a));
        prop_assert!(dominates(&m, &b));
    }

    #[test]
    fn merge_is_commutative(a in clock(), b in clock()) {
        prop_assert_eq!(merged(&a, &b).compare(&merged(&b, &a)), Order::Equal);
    }

    #[test]
    fn merge_is_associative(a in clock(), b in clock(), c in clock()) {
        let left = merged(&merged(&a, &b), &c);
        let right = merged(&a, &merged(&b, &c));
        prop_assert_eq!(left.compare(&right), Order::Equal);
    }

    #[test]
    fn merge_is_idempotent(a in clock()) {
        prop_assert_eq!(&merged(&a, &a), &a);
    }

    #[test]
    fn merge_is_the_least_upper_bound(a in clock(), b in clock(), c in clock()) {
        if dominates(&c, &a) && dominates(&c, &b) {
            prop_assert!(dominates(&c, &merged(&a, &b)));
        }
    }

    #[test]
    fn merge_grows_to_the_longer_length(a in clock(), b in clock()) {
        prop_assert_eq!(
            merged(&a, &b).entries().len(),
            a.entries().len().max(b.entries().len())
        );
    }

    #[test]
    fn tick_moves_the_clock_after(
        entries in prop::collection::vec(0u64..4, 1..6),
        pick in any::<prop::sample::Index>(),
    ) {
        let old = Clock::from_entries(entries);
        let mut new = old.clone();
        new.tick(pick.index(old.entries().len()));
        prop_assert_eq!(new.compare(&old), Order::After);
        prop_assert_eq!(old.compare(&new), Order::Before);
    }

    #[test]
    fn ticks_on_different_slots_are_concurrent(
        entries in prop::collection::vec(0u64..4, 2..6),
        i in any::<prop::sample::Index>(),
        j in any::<prop::sample::Index>(),
    ) {
        let n = entries.len();
        let (i, j) = (i.index(n), j.index(n));
        prop_assume!(i != j);
        let base = Clock::from_entries(entries);
        let (mut x, mut y) = (base.clone(), base);
        x.tick(i);
        y.tick(j);
        prop_assert_eq!(x.compare(&y), Order::Concurrent);
    }

    #[test]
    fn bytes_roundtrip(a in clock()) {
        let bytes = a.to_bytes();
        prop_assert_eq!(bytes.len(), 8 * a.entries().len());
        prop_assert_eq!(Clock::from_bytes(&bytes), Ok(a));
    }

    #[test]
    fn roundtrip_preserves_large_values(entries in prop::collection::vec(any::<u64>(), 0..6)) {
        let a = Clock::from_entries(entries);
        prop_assert_eq!(Clock::from_bytes(&a.to_bytes()), Ok(a));
    }

    #[test]
    fn from_bytes_rejects_non_multiples_of_eight(bytes in prop::collection::vec(any::<u8>(), 0..40)) {
        let result = Clock::from_bytes(&bytes);
        prop_assert_eq!(result.is_err(), !bytes.len().is_multiple_of(8));
    }

    #[test]
    fn padding_with_zeros_does_not_change_the_clock(a in clock(), extra in 0usize..4) {
        let mut entries = a.entries().to_vec();
        entries.resize(entries.len() + extra, 0);
        prop_assert_eq!(a.compare(&Clock::from_entries(entries)), Order::Equal);
    }
}
