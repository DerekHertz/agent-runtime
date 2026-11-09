//! Released versions (placeholder).
//!
//! Will track each file's released version so a Read of a file someone else holds the lease on can
//! be redirected to it (the `updated_input_json` of an Admission, SPEC section 4). The first
//! slice serves no snapshots.
