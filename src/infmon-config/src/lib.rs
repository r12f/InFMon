// Re-export everything from infmon-common::config so downstream crates
// continue to compile until they are migrated (see issue #102).
pub use infmon_common::config::*;
pub use infmon_common::config::{crud, model, parse, validate};
