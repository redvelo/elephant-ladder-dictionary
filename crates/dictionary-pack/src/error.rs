use std::{io, path::PathBuf};
use thiserror::Error;

/// A dictionary pack contract or storage error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PackError {
    /// A pack identifier does not satisfy the path-safe format contract.
    #[error("invalid pack identifier: {0}")]
    InvalidPackId(String),
    /// `SQLite` could not initialize or access a pack database.
    #[error("dictionary pack database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// A pack could not be read from storage.
    #[error("dictionary pack I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The pack does not conform to the declared format contract.
    #[error("malformed dictionary pack: {0}")]
    Malformed(String),
    /// Immutable bytes or redundant stored facts disagree.
    #[error("corrupt dictionary pack: {0}")]
    Corrupt(String),
    /// A caller-supplied operation exceeds configured runtime limits.
    #[error("dictionary pack limit exceeded: {0}")]
    Limit(String),
}
