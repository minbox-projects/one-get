//! Download engine foundations: typed errors, lifecycle state machines, safe
//! HTTP transport, and non-destructive probing.

pub mod error;
pub mod http;
pub mod probe;
pub mod state;
