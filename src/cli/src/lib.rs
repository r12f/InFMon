use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

pub mod exit_codes;
pub mod output;

/// infmonctl — operator CLI for InFMon
///
/// Single entry point for installing, running, configuring, observing,
/// and debugging InFMon on a BlueField-3 DPU.
#[derive(Parser, Debug)]
#[command(name = "infmonctl", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output format
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,

    /// Shorthand for --output json
    #[arg(long, global = true, default_value_t = false)]
    pub json: bool,

    /// JSON only: collapse pretty-printed output to a single line per record
    #[arg(long, global = true, default_value_t = false)]
    pub compact: bool,

    /// Table mode only: print byte counters as raw integers
    #[arg(long, global = true, default_value_t = false)]
    pub raw_bytes: bool,

    /// Override config file location
    #[arg(long, global = true, default_value = "/etc/infmon/config.yaml")]
    pub config: String,

    /// Override frontend socket path
    #[arg(long, global = true, default_value = "/run/infmon/frontend.sock")]
    pub socket: String,

    /// Per-call timeout when contacting the frontend (seconds)
    #[arg(long, global = true, default_value_t = 5)]
    pub timeout: u64,

    /// Disable ANSI colors
    #[arg(long, global = true, default_value_t = false)]
    pub no_color: bool,

    /// Suppress informational stderr
    #[arg(short, long, global = true, default_value_t = false)]
    pub quiet: bool,

    /// Verbosity level (-v info, -vv debug, -vvv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

impl Cli {
    /// Resolve the effective output format (--json overrides --output)
    pub fn effective_output(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.output.clone()
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Bootstrap InFMon installation
    Install {
        /// Proceed even when prior state looks dirty
        #[arg(long)]
        force: bool,
    },
    /// Remove InFMon installation
    Uninstall {
        /// Also remove /etc/infmon/ and /var/lib/infmon/
        #[arg(long)]
        purge: bool,
    },
    /// Start infmon-frontend service
    Start,
    /// Stop infmon-frontend service
    Stop,
    /// Restart infmon-frontend service
    Restart,
    /// Show composite service status
    Status,
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Flow-rule management (configured matchers)
    FlowRule {
        #[command(subcommand)]
        command: FlowRuleCommands,
    },
    /// Live flow inspection (read-only)
    Flow {
        #[command(subcommand)]
        command: FlowCommands,
    },
    /// Aggregate statistics
    Stats {
        #[command(subcommand)]
        command: StatsCommands,
    },
    /// Log access
    Log {
        #[command(subcommand)]
        command: LogCommands,
    },
    /// Composite health probe
    Health,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Get a single config key value
    Get {
        /// Dotted key path (e.g. exporter.otlp.endpoint)
        key: String,
    },
    /// Set a config key value (requires root)
    Set {
        /// Dotted key path
        key: String,
        /// New value
        value: String,
        /// Force value type when ambiguous
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
    },
    /// Reload configuration (requires root)
    Reload,
    /// Show effective merged configuration
    Show {
        /// Add restart-required annotations
        #[arg(long)]
        annotate: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum FlowRuleCommands {
    /// Add a flow rule (requires root)
    Add {
        /// Flow-rule spec in key=value form
        #[arg(required = true, num_args = 1..)]
        spec: Vec<String>,
    },
    /// Remove a flow rule (requires root)
    Rm {
        /// Rule ID, name, or spec to remove
        target: String,
        /// Delete all matching rules when spec matches multiple
        #[arg(long)]
        all: bool,
    },
    /// List all flow rules
    List,
    /// Show detailed info for a flow rule
    Show {
        /// Rule ID or name
        target: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum FlowCommands {
    /// List active flows under a flow-rule
    List {
        /// Rule ID or name
        rule: String,
        /// Show top N flows
        #[arg(long, default_value_t = 50)]
        top: usize,
        /// Sort by field
        #[arg(long, value_enum, default_value_t = FlowSortField::Bytes)]
        sort: FlowSortField,
    },
    /// Show a single flow's full counters
    Show {
        /// Rule ID or name
        rule: String,
        /// Canonical key tuple (single quoted argument)
        key: String,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum FlowSortField {
    Bytes,
    Packets,
    LastSeen,
}

#[derive(Subcommand, Debug)]
pub enum StatsCommands {
    /// Show aggregate counters
    Show {
        /// Restrict to a specific metric (supports globs)
        #[arg(long)]
        name: Option<String>,
        /// Top-N entries
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Periodic refresh interval in seconds
        #[arg(long)]
        watch: Option<u64>,
    },
    /// Export stats snapshot in a specific format
    Export {
        /// Export format
        #[arg(long, value_enum)]
        format: ExportFormat,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum ExportFormat {
    Prom,
    Otlp,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum LogCommands {
    /// Tail frontend logs (wraps journalctl)
    Tail {
        /// Follow new log entries
        #[arg(short, long)]
        follow: bool,
        /// Show logs since (journalctl format)
        #[arg(long)]
        since: Option<String>,
        /// Number of lines (default 100)
        #[arg(short, default_value_t = 100)]
        n: u64,
    },
}

// -----------------------------------------------------------------------
// Shell completion and man page generation helpers
// -----------------------------------------------------------------------

/// Generate shell completions for the given shell and write to stdout.
pub fn generate_completions(shell: &str) {
    use clap_complete::{generate, Shell};
    use std::io::Write;
    let shell: Shell = shell.parse().unwrap_or_else(|_| {
        eprintln!("infmonctl: unsupported shell: {shell}");
        std::process::exit(exit_codes::EXIT_USAGE);
    });
    let mut cmd = Cli::command();
    let mut out = std::io::stdout().lock();
    generate(shell, &mut cmd, "infmonctl", &mut out);
    out.flush().unwrap_or_else(|e| {
        eprintln!("infmonctl: failed to write completions: {e}");
        std::process::exit(exit_codes::EXIT_FAILURE);
    });
}

/// Generate a man page and write to stdout.
pub fn generate_manpage() {
    use std::io::Write;
    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut out = std::io::stdout().lock();
    man.render(&mut out).unwrap_or_else(|e| {
        eprintln!("infmonctl: failed to generate man page: {e}");
        std::process::exit(exit_codes::EXIT_FAILURE);
    });
    out.flush().unwrap_or_else(|e| {
        eprintln!("infmonctl: failed to flush man page output: {e}");
        std::process::exit(exit_codes::EXIT_FAILURE);
    });
}
