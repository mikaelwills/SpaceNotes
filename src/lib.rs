//! Library surface, deliberately minimal.
//!
//! The daemon is a binary; this exists so integration tests can start the file
//! server in-process on an ephemeral port and run the same expectation table
//! that `tests/files_http.rs` runs against nginx. Only modules that are
//! genuinely standalone belong here — `files_http` needs no SpacetimeDB
//! connection, journal or watcher.

pub mod files_http;
