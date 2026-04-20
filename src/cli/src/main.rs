use std::process;

use clap::Parser;

use infmon_cli::exit_codes::*;
use infmon_cli::{
    Cli, Commands, ConfigCommands, FlowCommands, FlowRuleCommands, LogCommands, StatsCommands,
};

fn main() {
    // Handle --generate-completions and --generate-manpage before clap
    // parsing, since these flags are used without a subcommand.
    let args: Vec<String> = std::env::args().collect();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--generate-completions" {
            let shell = args.get(i + 1).map(|s| s.as_str()).unwrap_or_else(|| {
                eprintln!("infmonctl: --generate-completions requires a shell argument");
                process::exit(EXIT_USAGE);
            });
            infmon_cli::generate_completions(shell);
            process::exit(EXIT_SUCCESS);
        }
        if arg.starts_with("--generate-completions=") {
            let shell = arg.trim_start_matches("--generate-completions=");
            infmon_cli::generate_completions(shell);
            process::exit(EXIT_SUCCESS);
        }
        if arg == "--generate-manpage" {
            infmon_cli::generate_manpage();
            process::exit(EXIT_SUCCESS);
        }
    }
    // Install SIGPIPE handler: exit 0 silently (spec 007 requirement).
    // We use SIG_IGN so writes to a closed pipe return EPIPE instead of
    // killing the process.  The write-error paths already cause the CLI
    // to exit; we just need to make sure the exit code is 0, not 141.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("infmonctl: failed to start runtime: {e}");
            process::exit(EXIT_FAILURE);
        });

    let code = rt.block_on(async {
        // Install SIGINT and SIGTERM handlers (spec 007: exit 130 / 143).
        // Create the signal streams eagerly so the OS-level handler is
        // registered before any subcommand runs — this avoids races where
        // the process receives a signal before the handler is polled.
        #[cfg(unix)]
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("failed to install SIGINT handler");
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        #[cfg(unix)]
        let sigint_task = tokio::spawn(async move {
            sigint.recv().await;
            process::exit(EXIT_SIGINT);
        });

        #[cfg(unix)]
        let sigterm_task = tokio::spawn(async move {
            sigterm.recv().await;
            process::exit(EXIT_SIGTERM);
        });

        let code = run(cli).await;

        #[cfg(unix)]
        sigint_task.abort();
        #[cfg(unix)]
        sigterm_task.abort();

        code
    });
    process::exit(code);
}

async fn run(cli: Cli) -> i32 {
    let _output_format = cli.effective_output();

    match cli.command {
        Commands::Install { force } => run_install(force).await,
        Commands::Uninstall { purge } => run_uninstall(purge).await,
        Commands::Start => run_lifecycle("start").await,
        Commands::Stop => run_lifecycle("stop").await,
        Commands::Restart => run_lifecycle("restart").await,
        Commands::Status => run_status(&cli).await,
        Commands::Config { ref command } => run_config(command, &cli).await,
        Commands::FlowRule { ref command } => run_flow_rule(command, &cli).await,
        Commands::Flow { ref command } => run_flow(command, &cli).await,
        Commands::Stats { ref command } => run_stats(command, &cli).await,
        Commands::Log { ref command } => run_log(command).await,
        Commands::Health => run_health(&cli).await,
    }
}

// ---------------------------------------------------------------------------
// Subcommand implementations (stubs — will await IPC client when wired)
// ---------------------------------------------------------------------------

async fn run_install(force: bool) -> i32 {
    let _ = force;
    // Check privilege
    if !is_root() {
        eprintln!("infmonctl: install requires root privileges");
        return EXIT_PERMISSION_DENIED;
    }
    eprintln!("infmonctl: install: not yet implemented (stub)");
    EXIT_FAILURE
}

async fn run_uninstall(purge: bool) -> i32 {
    let _ = purge;
    if !is_root() {
        eprintln!("infmonctl: uninstall requires root privileges");
        return EXIT_PERMISSION_DENIED;
    }
    eprintln!("infmonctl: uninstall: not yet implemented (stub)");
    EXIT_FAILURE
}

async fn run_lifecycle(action: &str) -> i32 {
    if !is_root() {
        eprintln!("infmonctl: {action} requires root privileges");
        return EXIT_PERMISSION_DENIED;
    }
    eprintln!("infmonctl: {action}: not yet implemented (stub)");
    EXIT_FAILURE
}

async fn run_status(cli: &Cli) -> i32 {
    let _ = cli;
    eprintln!("infmonctl: status: not yet implemented (stub)");
    EXIT_FAILURE
}

async fn run_config(cmd: &ConfigCommands, cli: &Cli) -> i32 {
    match cmd {
        ConfigCommands::Get { ref key } => {
            let _ = (key, cli);
            eprintln!("infmonctl: config get: not yet implemented (stub)");
            EXIT_FAILURE
        }
        ConfigCommands::Set {
            ref key,
            ref value,
            ref r#type,
        } => {
            if !is_root() {
                eprintln!("infmonctl: config set requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }
            let _ = (key, value, r#type, cli);
            eprintln!("infmonctl: config set: not yet implemented (stub)");
            EXIT_FAILURE
        }
        ConfigCommands::Reload => {
            if !is_root() {
                eprintln!("infmonctl: config reload requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }
            eprintln!("infmonctl: config reload: not yet implemented (stub)");
            EXIT_FAILURE
        }
        ConfigCommands::Show { annotate } => {
            let _ = (annotate, cli);
            eprintln!("infmonctl: config show: not yet implemented (stub)");
            EXIT_FAILURE
        }
    }
}

async fn run_flow_rule(cmd: &FlowRuleCommands, cli: &Cli) -> i32 {
    match cmd {
        FlowRuleCommands::Add { ref spec } => {
            if !is_root() {
                eprintln!("infmonctl: flow-rule add requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }
            let _ = (spec, cli);
            eprintln!("infmonctl: flow-rule add: not yet implemented (stub)");
            EXIT_FAILURE
        }
        FlowRuleCommands::Rm { ref target, all } => {
            if !is_root() {
                eprintln!("infmonctl: flow-rule rm requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }
            let _ = (target, all, cli);
            eprintln!("infmonctl: flow-rule rm: not yet implemented (stub)");
            EXIT_FAILURE
        }
        FlowRuleCommands::List => {
            let _ = cli;
            eprintln!("infmonctl: flow-rule list: not yet implemented (stub)");
            EXIT_FAILURE
        }
        FlowRuleCommands::Show { ref target } => {
            let _ = (target, cli);
            eprintln!("infmonctl: flow-rule show: not yet implemented (stub)");
            EXIT_FAILURE
        }
    }
}

async fn run_flow(cmd: &FlowCommands, cli: &Cli) -> i32 {
    match cmd {
        FlowCommands::List {
            ref rule,
            top,
            ref sort,
        } => {
            let _ = (rule, top, sort, cli);
            eprintln!("infmonctl: flow list: not yet implemented (stub)");
            EXIT_FAILURE
        }
        FlowCommands::Show { ref rule, ref key } => {
            let _ = (rule, key, cli);
            eprintln!("infmonctl: flow show: not yet implemented (stub)");
            EXIT_FAILURE
        }
    }
}

async fn run_stats(cmd: &StatsCommands, cli: &Cli) -> i32 {
    match cmd {
        StatsCommands::Show {
            ref name,
            top,
            ref watch,
        } => {
            let _ = (name, top, watch, cli);
            eprintln!("infmonctl: stats show: not yet implemented (stub)");
            EXIT_FAILURE
        }
        StatsCommands::Export { ref format } => {
            let _ = (format, cli);
            eprintln!("infmonctl: stats export: not yet implemented (stub)");
            EXIT_FAILURE
        }
    }
}

async fn run_log(cmd: &LogCommands) -> i32 {
    match cmd {
        LogCommands::Tail {
            follow,
            ref since,
            n,
        } => {
            let _ = (since, n);
            if *follow {
                eprintln!("infmonctl: log tail: waiting for logs (stub)");
                // Block indefinitely — sleep long enough for signal handlers
                // to fire. Uses tokio::time so the I/O driver (which polls
                // for signals) is properly driven.
                tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
                return EXIT_FAILURE;
            }
            eprintln!("infmonctl: log tail: not yet implemented (stub)");
            EXIT_FAILURE
        }
    }
}

async fn run_health(cli: &Cli) -> i32 {
    let _ = cli;
    eprintln!("infmonctl: health: not yet implemented (stub)");
    EXIT_FAILURE
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
