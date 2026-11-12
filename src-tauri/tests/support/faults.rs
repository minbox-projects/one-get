//! Deterministic filesystem fault injection for engine tests.
//!
//! The library hook is keyed by part filename so tests that run in parallel do
//! not interfere with each other. This guard clears the fault on drop, even
//! when an assertion fails.

use oneget_lib::engine::part_file::{clear_test_write_fault, set_test_write_fault};

/// Injects a bounded "disk full" write failure into one part file.
pub struct DiskFullFault<'a> {
    part_name: &'a str,
}

impl<'a> DiskFullFault<'a> {
    /// Fail writes after `fail_after_bytes` for the named part file.
    pub fn install(part_name: &'a str, fail_after_bytes: u64) -> Self {
        set_test_write_fault(part_name, fail_after_bytes);
        Self { part_name }
    }
}

impl Drop for DiskFullFault<'_> {
    fn drop(&mut self) {
        clear_test_write_fault(self.part_name);
    }
}
