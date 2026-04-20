use std::process;

use clap::Parser;

use infmon_cli::exit_codes::*;
use infmon_cli::{
    Cli, Commands, ConfigCommands, FlowCommands, FlowRuleCommands, LogCommands, StatsCommands,
};

fn main() {
    // Install SIGPIPE handler: exit 0 silently (spec requirement)
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("infmonctl: failed to start runtime: {e}");
            process::exit(EXIT_FAILURE);
        });

    let code = rt.block_on(async { run(cli).await });
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
            let _ = (follow, since, n);
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
