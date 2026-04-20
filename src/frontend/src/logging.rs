//! Logging initialization for infmon-frontend.
//!
//! Builds a `tracing` subscriber based on [`LoggingConfig`], supporting
//! syslog and file destinations with `RUST_LOG` env-var override.

use infmon_common::config::{LogLevel, LogType, LoggingConfig, Rotation};
use syslog_tracing::{Facility, Options, Syslog};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

/// Guard that must be held for the lifetime of the program.
/// Dropping it flushes any buffered log output (file and syslog appenders).
#[derive(Debug)]
pub struct LoggingGuard {
    _guards: Vec<WorkerGuard>,
}

/// Initialize a bootstrap stderr subscriber for use during config parsing.
/// Call this early, before config is available.  Returns a
/// [`DefaultGuard`](tracing::subscriber::DefaultGuard) that sets the
/// subscriber as the **thread-local** default.  Drop the guard before (or
/// immediately after) calling [`init_logging`] so the global default can
/// be installed without conflict.
pub fn init_bootstrap() -> tracing::subscriber::DefaultGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .finish();
    // Thread-local default — does not consume the one-shot global slot.
    tracing::subscriber::set_default(subscriber)
}

/// Initialize the configured logging subscriber.
///
/// # Errors
/// Returns an error if the syslog or file appender cannot be created.
pub fn init_logging(config: &LoggingConfig) -> Result<LoggingGuard, Box<dyn std::error::Error>> {
    let default_level = match config.level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };

    // RUST_LOG overrides config level
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    match config.destination {
        LogType::Syslog => {
            let identity = c"infmon";
            let syslog = Syslog::new(identity, Options::LOG_PID, Facility::Daemon)
                .ok_or("failed to initialize syslog")?;

            // NOTE: syslog_tracing::Syslog implements MakeWriter but not
            // std::io::Write, so it cannot be wrapped with
            // tracing_appender::non_blocking.  Syslog writes go to a Unix
            // domain socket which is typically fast (kernel-buffered), so
            // blocking here is acceptable for normal log volumes.
            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(filter)
                .with_writer(syslog)
                .with_ansi(false)
                .finish();

            tracing::subscriber::set_global_default(subscriber)
                .map_err(|e| format!("failed to set global subscriber: {e}"))?;

            Ok(LoggingGuard { _guards: vec![] })
        }
        LogType::File => {
            let file_config = config
                .file
                .as_ref()
                .ok_or("logging destination is 'file' but no file config provided")?;

            let path = std::path::Path::new(&file_config.path);
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("infmon.log");

            // NOTE: tracing_appender::rolling does not limit the number of
            // old log files.  For production use, configure external log
            // rotation (e.g. logrotate) or switch to
            // RollingFileAppender::builder().max_log_files(N) when
            // tracing-appender 0.2.3+ is available.
            let rotation = match file_config.rotation {
                Rotation::Daily => tracing_appender::rolling::daily(dir, filename),
                Rotation::Hourly => tracing_appender::rolling::hourly(dir, filename),
                Rotation::Never => tracing_appender::rolling::never(dir, filename),
            };

            let (non_blocking, guard) = tracing_appender::non_blocking(rotation);

            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(filter)
                .with_writer(non_blocking)
                .with_ansi(false)
                .finish();

            tracing::subscriber::set_global_default(subscriber)
                .map_err(|e| format!("failed to set global subscriber: {e}"))?;

            Ok(LoggingGuard {
                _guards: vec![guard],
            })
        }
    }
}

#[cfg(test)]
#[path = "logging_tests.rs"]
mod tests;
