# 009 — Logging system design (tracing + syslog/file)

## Version history

| Version | Date       | Author       | Changes        |
| ------- | ---------- | ------------ | -------------- |
| 0.1     | 2026-04-20 | Riff (r12f)  | Initial draft. |

- **Depends on:** [`005-frontend-architecture`](005-frontend-architecture.md), [`007-cli`](007-cli.md)
- **Related:** [`008-packaging-install`](008-packaging-install.md)
- **Affects:** `infmon-frontend`, `infmon-cli`

---

## Context

InFMon needs structured, configurable logging across its components.
The frontend daemon requires production-grade log routing (syslog for
systemd-managed deployments, file for debugging and auditing) while the
CLI needs lightweight diagnostic output that does not interfere with
its user-facing `eprintln!` messages.

Library crates (`infmon-common`, `infmon-ipc`) must remain pure — no
logging subscriptions, no global state — so they can be embedded in
any context without side effects.

## Goals & Non-goals

- **Goal:** Use the `tracing` ecosystem uniformly across the frontend
  and CLI, replacing any use of `log` + `env_logger`.
- **Goal:** Support syslog and file destinations for the frontend,
  selectable via YAML config.
- **Goal:** Allow `RUST_LOG` env-var to override the configured log
  level for ad-hoc debugging.
- **Goal:** Add a `-v`/`--verbose` flag to the CLI for stderr-based
  debug tracing.
- **Non-goal:** Structured JSON log output (may be added later as a
  destination variant).
- **Non-goal:** Remote log shipping (e.g. to a log aggregator) — out
  of scope for v1.
- **Non-goal:** Logging from library crates (`infmon-common`,
  `infmon-ipc`).

## Design

### 1. Crate ecosystem

All logging uses the `tracing` family:

| Crate                | Purpose                              |
| -------------------- | ------------------------------------ |
| `tracing`            | Instrumentation API (spans, events)  |
| `tracing-subscriber` | Subscriber composition, `EnvFilter`  |
| `tracing-appender`   | File rotation, non-blocking writer   |
| `syslog-tracing`     | Syslog destination (Unix socket)     |

### 2. Per-crate strategy

| Crate            | Logging approach                                                    |
| ---------------- | ------------------------------------------------------------------- |
| `infmon-frontend`| Full tracing subscriber configured from YAML. Bootstrap stderr subscriber during config parse, switch to configured subscriber after. |
| `infmon-cli`     | No subscriber by default. `-v`/`--verbose` flag enables a stderr subscriber at debug level. All user-facing output remains `eprintln!`. |
| `infmon-common`  | No logging — pure library.                                         |
| `infmon-ipc`     | No logging — pure library.                                         |

### 3. Configuration model

The `LoggingConfig` struct lives in `infmon-common::config::model` and
is part of the top-level YAML configuration:

```yaml
logging:
  level: info          # trace | debug | info | warn | error
  destination: syslog  # syslog | file
  file:
    path: /var/log/infmon/infmon.log
    rotation: daily    # daily | hourly | never
```

Rust types:

```rust
pub struct LoggingConfig {
    pub level: LogLevel,       // default: Info
    pub destination: LogType,  // default: Syslog
    pub file: Option<LogFileConfig>,
}

pub struct LogFileConfig {
    pub path: String,
    pub rotation: Rotation,    // default: Daily
}
```

**`RUST_LOG` override:** When the `RUST_LOG` environment variable is
set, it takes precedence over `config.logging.level`.  This is
implemented via `EnvFilter::try_from_default_env()` with a fallback
to the configured level.

### 4. Frontend logging lifecycle

1. **Bootstrap phase** — Before config is parsed, `logging::init_bootstrap()`
   installs a thread-local stderr subscriber at `info` level (or
   `RUST_LOG` if set). This ensures early startup errors are visible.
   Returns a `DefaultGuard`; dropping it removes the thread-local
   subscriber.

2. **Configured phase** — After config is parsed successfully,
   `logging::init_logging(&config.logging)` installs the global
   subscriber matching the configured destination. The bootstrap
   guard is dropped at this point.

3. **Guard pattern** — `init_logging` returns a `LoggingGuard` that
   holds any `WorkerGuard` instances (e.g. the non-blocking file
   appender guard). This guard must be held until process exit to
   ensure all buffered output is flushed.

### 5. Syslog destination

- Uses `syslog_tracing::Syslog` with:
  - Identity: `"infmon"` (as a C string literal `c"infmon"`)
  - Options: `LOG_PID`
  - Facility: `Daemon`
- Syslog writes go to a Unix domain socket (kernel-buffered), so
  blocking I/O is acceptable for normal log volumes.
- No `WorkerGuard` needed — the `LoggingGuard._guards` vector is empty.

### 6. File destination

- Uses `tracing-appender` rolling file appender.
- Rotation options: `daily`, `hourly`, `never`.
- Wrapped in `tracing_appender::non_blocking` for async-safe writes.
- The `WorkerGuard` from the non-blocking wrapper is stored in
  `LoggingGuard` to guarantee flush-on-drop.
- **Log retention:** `tracing-appender` does not limit old log files.
  For production, configure external log rotation (e.g. `logrotate`)
  or use `RollingFileAppender::builder().max_log_files(N)` when
  available in a future `tracing-appender` release.

### 7. CLI verbose flag

The CLI (`infmonctl`) adds a `-v`/`--verbose` flag via `clap`:

```rust
#[arg(short, long)]
verbose: bool,
```

When set, the CLI initializes a minimal stderr tracing subscriber:

```rust
tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_max_level(Level::DEBUG)
    .init();
```

Key diagnostic points that emit `tracing::debug!`:

- Connecting to the IPC socket
- Sending a command
- Receiving a response

Without `-v`, no tracing subscriber is installed and all `tracing`
macros are no-ops. Existing `eprintln!` calls for user-facing output
remain unchanged.

## Interfaces

### Config YAML schema

```yaml
logging:                      # optional, defaults applied if absent
  level: <LogLevel>           # optional, default: info
  destination: <LogType>      # optional, default: syslog
  file:                       # required when destination=file
    path: <string>            # required
    rotation: <Rotation>      # optional, default: daily
```

### Public API (`infmon-frontend::logging`)

```rust
/// Install a bootstrap stderr subscriber (thread-local).
pub fn init_bootstrap() -> tracing::subscriber::DefaultGuard;

/// Install the configured global subscriber.
pub fn init_logging(config: &LoggingConfig) -> Result<LoggingGuard, Box<dyn Error>>;
```

### CLI flag

```text
infmonctl [OPTIONS] <COMMAND>

Options:
  -v, --verbose    Enable debug tracing output on stderr
```

## Test plan

- **Unit tests** (`logging_tests.rs`):
  - `init_bootstrap` returns a guard and does not panic.
  - `init_logging` with syslog config succeeds.
  - `init_logging` with file config creates the appender and returns
    a guard with a non-empty `_guards` vector.
  - `init_logging` with file destination but missing `file` config
    returns an error.
  - `RUST_LOG` override is respected when set.

- **CLI tests** (`assert_cmd`):
  - `infmonctl --help` mentions `-v`/`--verbose`.
  - `infmonctl -v <command>` produces debug output on stderr
    (when a valid IPC socket is available).

- **Integration:**
  - Frontend starts with syslog destination — verify log lines appear
    in `journalctl` / syslog.
  - Frontend starts with file destination — verify log file is created
    at the configured path with expected rotation.

## Open questions

1. **Structured JSON output** — Should we add a `json` format option
   alongside plain text? *Proposed default: defer to a future spec.*

2. **Max log files for file rotation** — Should we enforce a cap on
   retained log files in-process, or rely on external `logrotate`?
   *Proposed default: rely on external `logrotate` for v1.*
