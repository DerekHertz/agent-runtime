//! The lease table (placeholder).
//!
//! Will hold each file's lease holder, each agent's Invalid/Shared/Owned standing toward it, and
//! the release and latest-write clocks of SPEC sections 4 and 5, plus the conflict rules for
//! whole-tree leases (ADR 0001). The first slice grants no leases: every call is allowed.
