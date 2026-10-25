#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

pub mod build;
pub mod ddl;
pub mod diff;
pub mod query;
pub mod schema_source;
pub mod subset;
pub mod views;

pub use build::{build, refresh, upgrade, BuildOptions, BuildProgress, BuildStats, ProgressFn, TableStats};
pub use ddl::IndexLevel;
pub use diff::{diff_databases, DiffStats, EntityDiff};
pub use query::{Database, OpenOptions, Pooled, Session};
pub use schema_source::{resolve_schema, SchemaOrigin};

/// The format of the databases this crate writes; stored in `mb_meta`.
/// Bumped when a database built by an older release can no longer be read
/// the same way.
pub const DATABASE_FORMAT: u32 = 1;

/// Errors building, diffing or querying a database.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading the dump.
    #[error(transparent)]
    Dump(#[from] musicbrainz_dump::DumpError),
    /// DuckDB.
    #[error("DuckDB: {0}")]
    DuckDb(#[from] duckdb::Error),
    /// The schema.
    #[error("schema: {0}")]
    Schema(String),
    /// The file system.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// What the caller asked for is not possible.
    #[error("{0}")]
    Invalid(String),
    /// Stopped by the caller.
    #[error("stopped")]
    Stopped,
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;
