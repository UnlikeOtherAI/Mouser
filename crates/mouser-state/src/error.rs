//! Error type for [`crate::SharedState`].

use std::fmt;

/// Errors raised by the shared-state CRDT wrapper.
///
/// The runtime paths are panic-free (the workspace clippy lints deny
/// `unwrap_used`/`panic`/`indexing_slicing`), so every fallible automerge
/// operation surfaces here instead of unwinding.
#[derive(Debug)]
pub enum StateError {
    /// An underlying automerge operation failed (read, write, save, load).
    Automerge(automerge::AutomergeError),
    /// A `StateSnapshot`/`StateChanges` byte payload could not be decoded.
    Decode(String),
    /// The document is structurally invalid (e.g. a root key holds the wrong
    /// automerge object type). Indicates a corrupt or hostile payload.
    Schema(String),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Automerge(e) => write!(f, "automerge error: {e}"),
            StateError::Decode(m) => write!(f, "decode error: {m}"),
            StateError::Schema(m) => write!(f, "schema error: {m}"),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StateError::Automerge(e) => Some(e),
            StateError::Decode(_) | StateError::Schema(_) => None,
        }
    }
}

impl From<automerge::AutomergeError> for StateError {
    fn from(e: automerge::AutomergeError) -> Self {
        StateError::Automerge(e)
    }
}

/// Convenience result alias for the crate's public API.
pub type StateResult<T> = Result<T, StateError>;
