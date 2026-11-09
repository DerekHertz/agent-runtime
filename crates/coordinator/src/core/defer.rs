//! Deferred admissions (placeholder).
//!
//! Will hold the calls the coordinator is holding until a lease frees up, and the rule that turns
//! a Defer into a Deny when its timeout passes (SPEC sections 3 and 4). Time enters only through
//! the `now` argument of `step`, never from a clock read. The first slice never defers.
