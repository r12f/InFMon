# 009 — Logging system design (tracing + syslog/file)

## Version history

| Version | Date       | Author       | Changes        |
| ------- | ---------- | ------------ | -------------- |
| 0.1     | 2026-04-20 | Riff (r12f)  | Initial draft. |
| 0.2     | 2026-04-20 | Riff (r12f)  | Address review: add enum defs, PathBuf, error contract, format, directory creation, naming, validation, SIGHUP, max_log_files, RUST_LOG directives, bootstrap threading, backpressure note. |

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
    max_files: 7       # max rotated files to retain (0 = unlimited)
```

Rust types:

```rust
/// Log severity level, maps 1:1 to `tracing::Level`.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Log output destination.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogType {
    #[default]
    Syslog,
    File,
}

/// File rotation strategy, maps to `tracing_appender::rolling::Rotation`.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rotation {
    #[default]
    Daily,
    Hourly,
    Never,
}

pub struct LoggingConfig {
    pub level: LogLevel,       // default: Info
    pub destination: LogType,  // default: Syslog
    pub file: Option<LogFileConfig>,
}

pub struct LogFileConfig {
    pub path: PathBuf,
    pub rotation: Rotation,    // default: Daily
    pub max_files: usize,      // default: 7
}
```

**Config validation:** When `destination` is `File`, the `file` section
is required. This is enforced during config deserialization/validation
(not deferred to `init_logging`), producing a clear error message like
`"logging.file is required when destination is 'file'"`. A missing
`file` section with `destination: syslog` is fine (`file` is ignored).

**Path resolution:** `LogFileConfig.path` is a `PathBuf`. Relative
paths are resolved against the working directory of the process. In
practice, the packaging spec should set an absolute path in the default
config shipped with the `.deb`. The spec does not mandate canonicalization.

**`RUST_LOG` override:** When the `RUST_LOG` environment variable is
set, it takes precedence over `config.logging.level`.  This is
implemented via `EnvFilter::try_from_default_env()` with a fallback
to the configured level. Note that `RUST_LOG` supports full directive
syntax (e.g. `infmon_frontend=debug,hyper=warn`), not just a bare
level — this allows per-crate filtering for ad-hoc debugging.

### 4. Frontend logging lifecycle

1. **Bootstrap phase** — Before config is parsed, `logging::init_bootstrap()`
   installs a **global** subscriber at `info` level (or `RUST_LOG` if
   set) using `tracing::subscriber::set_global_default()`. This ensures
   early startup errors are visible across all threads, including any
   async runtime threads spawned before config parsing completes.
   Returns a `BootstrapGuard` whose `Drop` impl is a no-op (the global
   subscriber cannot be removed, but will be replaced by the configured
   subscriber in the next phase).

2. **Configured phase** — After config is parsed successfully,
   `logging::init_logging(&config.logging)` replaces the global
   subscriber with the one matching the configured destination.

3. **Error contract** — If `init_logging` returns `Err`, the bootstrap
   subscriber remains active (it was installed globally and is not
   removed). The caller should log the error via the still-active
   bootstrap subscriber and then decide whether to abort startup or
   continue with degraded (stderr-only) logging. The recommended
   default is to abort with a clear error message.

4. **Guard pattern** — `init_logging` returns a `LoggingGuard` that
   holds any `WorkerGuard` instances (e.g. the non-blocking file
   appender guard). This guard must be held until process exit to
   ensure all buffered output is flushed.

### 5. Syslog destination

- Uses `syslog_tracing::Syslog` with:
  - Identity: `"infmon"` (as a C string literal `c"infmon"`)
  - Options: `LOG_PID`
  - Facility: `Daemon`
- Syslog writes go to a Unix domain socket (kernel-buffered), so
  blocking I/O is acceptable. Even under burst conditions (e.g. a
  flap storm generating hundreds of log lines per second), the
  kernel socket buffer absorbs the writes without blocking the event
  loop for meaningful durations. If this assumption proves wrong under
  production load, a non-blocking wrapper can be added as a follow-up.
- No `WorkerGuard` needed — the `LoggingGuard.guards` vector is empty.

### 6. File destination

- Uses `tracing-appender` rolling file appender.
- Rotation options: `daily`, `hourly`, `never`.
- **Max log files:** Uses `RollingFileAppender::builder().max_log_files(N)`
  to cap retained rotated files. Default: 7 (for daily rotation, one
  week of history). Set `max_files: 0` to disable the cap (rely on
  external `logrotate` instead).
- Wrapped in `tracing_appender::non_blocking` for async-safe writes.
- The `WorkerGuard` from the non-blocking wrapper is stored in
  `LoggingGuard` to guarantee flush-on-drop.
- **Directory creation:** `init_logging` calls
  `std::fs::create_dir_all()` on the parent directory of `path` before
  creating the appender. If directory creation fails (e.g. permission
  denied), `init_logging` returns `Err` with context.
- **Log line format:** File output uses `tracing_subscriber::fmt::format::Full`
  (the default `fmt` formatter), producing lines like:
  `2026-04-20T07:31:04.123456Z  INFO infmon_frontend::poller: polling interface eth0`.
  Each line includes: RFC 3339 timestamp, level, target (module path),
  and message. Span context is included when active. This can be changed
  to `json` format in a future spec if structured output is needed.
- **SIGHUP re-open:** Deferred. Since `tracing-appender`'s rolling
  appender manages rotation internally (opening new files on the
  configured schedule), external `logrotate` with `copytruncate` works
  without SIGHUP support. If a future requirement mandates
  `logrotate`-with-`create` (rename-and-signal), SIGHUP handling will
  be specified in a follow-up.

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
    path: <PathBuf>           # required
    rotation: <Rotation>      # optional, default: daily
    max_files: <usize>        # optional, default: 7
```

### Public API (`infmon-frontend::logging`)

```rust
/// Install a bootstrap global subscriber.
pub fn init_bootstrap() -> BootstrapGuard;

/// Install the configured global subscriber.
/// On error, the bootstrap subscriber remains active.
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
    a guard with a non-empty `guards` vector.
  - `init_logging` with file destination but missing `file` config
    is rejected at config validation (deserialization error), not at
    `init_logging` time.
  - `RUST_LOG` override is respected when set.
  - Log directory is created if it does not exist.
  - `init_logging` returns `Err` if directory creation fails
    (e.g. permission denied).

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
