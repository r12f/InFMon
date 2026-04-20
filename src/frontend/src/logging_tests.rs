//! Tests for the logging module.

use super::*;
use infmon_common::config::{LogFileConfig, LogLevel, LogType, LoggingConfig, Rotation};

#[test]
fn test_logging_guard_drops_without_panic() {
    let guard = LoggingGuard { _guard: None };
    drop(guard);
}

#[test]
fn test_init_logging_file_missing_config() {
    let config = LoggingConfig {
        level: LogLevel::Info,
        destination: LogType::File,
        file: None,
    };
    let result = init_logging(&config);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("no file config"), "unexpected error: {err}");
}

#[test]
fn test_file_destination_with_config() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("test.log");

    let config = LoggingConfig {
        level: LogLevel::Debug,
        destination: LogType::File,
        file: Some(LogFileConfig {
            path: log_path.to_str().unwrap().to_string(),
            rotation: Rotation::Never,
        }),
    };

    // May fail because a global subscriber is already set, but should not
    // fail on file creation.
    let result = init_logging(&config);
    match result {
        Ok(_guard) => {}
        Err(e) => {
            let msg = e.to_string();
            assert!(msg.contains("global subscriber"), "unexpected error: {msg}");
        }
    }
}
