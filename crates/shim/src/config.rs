//! Slot configuration from the environment (SPEC section 3, "Slots and launching").
//!
//! The launcher names the shim's slot and the coordinator's socket through two variables. The shim
//! keeps no state of its own, so these are the only inputs besides the hook's stdin.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{bail, Result};

pub const SLOT_VAR: &str = "AGENT_RUNTIME_SLOT";
pub const SOCKET_VAR: &str = "AGENT_RUNTIME_SOCKET";

/// Where the shim sends its calls. Present only inside a run.
#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub slot: String,
    pub socket: PathBuf,
}

/// Decide from the two variables whether this process belongs to a run.
///
/// - Neither set: not in a run, `Ok(None)`. The shim must then do nothing at all.
/// - Both set: `Ok(Some(..))`.
/// - Exactly one set: a half-configured launcher. This is an error so the caller fails closed;
///   silently doing nothing would let the call through unchecked.
///
/// An empty value counts as unset. Shells and launchers commonly export `VAR=` to mean "none", and
/// an empty slot or an empty socket path is unusable anyway, so treating it as present would only
/// turn an ordinary non-run session into a blocked one. A value that is set but not valid
/// Unicode (slot) is an error, not "unset", because it names a run we cannot honour.
pub fn from_vars(slot: Option<OsString>, socket: Option<OsString>) -> Result<Option<Config>> {
    let slot = slot.filter(|v| !v.is_empty());
    let socket = socket.filter(|v| !v.is_empty());
    match (slot, socket) {
        (None, None) => Ok(None),
        (Some(slot), Some(socket)) => {
            let Ok(slot) = slot.into_string() else {
                bail!("{SLOT_VAR} is not valid Unicode");
            };
            Ok(Some(Config {
                slot,
                socket: PathBuf::from(socket),
            }))
        }
        (Some(_), None) => bail!("{SLOT_VAR} is set but {SOCKET_VAR} is not"),
        (None, Some(_)) => bail!("{SOCKET_VAR} is set but {SLOT_VAR} is not"),
    }
}

/// Read the configuration from the process environment.
pub fn from_env() -> Result<Option<Config>> {
    from_vars(std::env::var_os(SLOT_VAR), std::env::var_os(SOCKET_VAR))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn neither_set_is_not_a_run() {
        assert_eq!(from_vars(None, None).unwrap(), None);
    }

    #[test]
    fn empty_values_count_as_unset() {
        assert_eq!(from_vars(os(""), os("")).unwrap(), None);
        assert_eq!(from_vars(os(""), None).unwrap(), None);
        assert_eq!(from_vars(None, os("")).unwrap(), None);
    }

    #[test]
    fn both_set_gives_config() {
        let c = from_vars(os("a"), os("/tmp/s.sock")).unwrap().unwrap();
        assert_eq!(c.slot, "a");
        assert_eq!(c.socket, PathBuf::from("/tmp/s.sock"));
    }

    #[test]
    fn exactly_one_set_is_an_error() {
        assert!(from_vars(os("a"), None).is_err());
        assert!(from_vars(None, os("/tmp/s.sock")).is_err());
        assert!(from_vars(os("a"), os("")).is_err());
        assert!(from_vars(os(""), os("/tmp/s.sock")).is_err());
    }
}
