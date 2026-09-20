//! Re-export of the closed JSON Schema subset for subagent structured output.
//!
//! The implementation and its tests live in `workflow-core/src/schema.rs`. This
//! path remains available so host and workflow callers share one compiler and
//! validation entry point.

pub use workflow_core::schema::*;
