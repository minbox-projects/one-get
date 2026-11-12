//! Download engine foundations: typed errors, lifecycle state machines, safe
//! HTTP transport, non-destructive probing, segment planning, part files, and
//! bounded worker execution.

pub mod error;
pub mod http;
pub mod part_file;
pub mod planner;
pub mod probe;
pub mod scheduler;
pub mod state;
pub mod worker;
