use std::path::Path;
use std::process;
use std::time::Duration;

use clap::Parser;

use infmon_cli::exit_codes::*;
use infmon_cli::output::{print_output, TableDisplay};
use infmon_cli::{
    Cli, Commands, ConfigCommands, FlowCommands, FlowRuleCommands, LogCommands, OutputFormat,
    StatsCommands,
};
use infmon_common::ipc::control_client::InFMonControlClient;
use infmon_common::ipc::protocol::{FlowRuleData, FlowRuleDetailData, StatsShowData};

fn main() {
    // Handle --generate-completions and --generate-manpage before clap
    // parsing, since these flags are used without a subcommand.
    // NOTE: We scan all args (not just args[1]) so that the flags work
    // regardless of position.  This is intentional — these are hidden
    // build-time helpers, not user-facing subcommands, so a positional
    // collision (e.g. `infmonctl install --generate-completions bash`)
    // is acceptable and extremely unlikely in practice.
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

    // Initialize tracing subscriber when -v is passed (spec: issue #121).
    // -v = INFO, -vv = DEBUG, -vvv = TRACE.  Output goes to stderr so it
    // never mixes with machine-readable stdout.  RUST_LOG overrides the
    // default level when set.  Existing eprintln! output is kept.
    if cli.verbose > 0 {
        use tracing_subscriber::EnvFilter;

        let level = match cli.verbose {
            1 => tracing::Level::INFO,
            2 => tracing::Level::DEBUG,
            _ => tracing::Level::TRACE,
        };
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new(level.as_str())),
            )
            .try_init()
            .ok();
    }

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

    tracing::debug!(
        command = cli.command.variant_name(),
        "dispatching subcommand"
    );

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
// Helper: create a control client from CLI args
// ---------------------------------------------------------------------------

fn make_client(cli: &Cli) -> InFMonControlClient {
    InFMonControlClient::with_timeout(Path::new(&cli.socket), Duration::from_secs(cli.timeout))
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

async fn run_install(force: bool) -> i32 {
    if !is_root() {
        eprintln!("infmonctl: install requires root privileges");
        return EXIT_PERMISSION_DENIED;
    }

    // Step 1: Create infmon system user and group (idempotent)
    if let Err(code) = ensure_system_user().await {
        return code;
    }

    // Step 2: Create config directory and write default config
    let config_dir = Path::new("/etc/infmon");
    let config_path = config_dir.join("config.yaml");
    if let Err(e) = std::fs::create_dir_all(config_dir) {
        eprintln!(
            "infmonctl: install: failed to create {}: {e}",
            config_dir.display()
        );
        return EXIT_FAILURE;
    }

    if config_path.exists() && !force {
        eprintln!(
            "infmonctl: install: skipping config (already exists at {}; use --force to overwrite)",
            config_path.display()
        );
    } else {
        let default_config = infmon_common::config::model::Config {
            frontend: Some(infmon_common::config::model::FrontendConfig::default()),
            flow_rules: vec![],
            exporters: None,
            logging: None,
        };
        match serde_yaml::to_string(&default_config) {
            Ok(yaml) => {
                let full_yaml = format!("# InFMon default configuration\n# See https://github.com/r12f/InFMon for documentation\n\n{yaml}");
                if let Err(e) = std::fs::write(&config_path, full_yaml) {
                    eprintln!(
                        "infmonctl: install: failed to write {}: {e}",
                        config_path.display()
                    );
                    return EXIT_FAILURE;
                }
                tracing::info!("wrote default config to {}", config_path.display());
                // Set restrictive permissions (0640 root:infmon)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o640);
                    if let Err(e) = std::fs::set_permissions(&config_path, perms) {
                        eprintln!(
                            "infmonctl: install: warning: failed to set permissions on {}: {e}",
                            config_path.display()
                        );
                    }
                    let _ =
                        run_cmd("chown", &["root:infmon", &config_path.to_string_lossy()]).await;
                }
            }
            Err(e) => {
                eprintln!("infmonctl: install: failed to serialize default config: {e}");
                return EXIT_FAILURE;
            }
        }
    }

    // Step 3: Create state and runtime directories
    for dir in &["/var/lib/infmon", "/run/infmon"] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("infmonctl: install: failed to create {dir}: {e}");
            return EXIT_FAILURE;
        }
        // chown to infmon:infmon
        match run_cmd("chown", &["infmon:infmon", dir]).await {
            Ok(out) if !out.status.success() => {
                eprintln!("infmonctl: install: warning: failed to chown {dir} to infmon:infmon");
            }
            Err(e) => {
                eprintln!("infmonctl: install: warning: chown {dir}: {e}");
            }
            _ => {}
        }
    }

    // Step 4: Install systemd unit file
    let service_src = Path::new("/usr/share/infmon/infmon.service");
    let service_dst = Path::new("/etc/systemd/system/infmon.service");

    // Try bundled service file first, fall back to deploy/ in source tree
    let service_content = if service_src.exists() {
        std::fs::read_to_string(service_src).ok()
    } else {
        // Try relative to the running binary.
        // Assumes layout: <prefix>/bin/infmonctl with <prefix>/deploy/infmon.service.
        // This is the standard layout from `cargo install` or the .deb package.
        std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.join("deploy/infmon.service"))
            })
            .and_then(|p| std::fs::read_to_string(p).ok())
    };

    if let Some(content) = service_content {
        if service_dst.exists() && !force {
            eprintln!(
                "infmonctl: install: service unit already exists at {}; use --force to overwrite",
                service_dst.display()
            );
        } else {
            if let Err(e) = std::fs::write(service_dst, content) {
                eprintln!(
                    "infmonctl: install: failed to write {}: {e}",
                    service_dst.display()
                );
                return EXIT_FAILURE;
            }
            // Reload systemd daemon
            match run_cmd("systemctl", &["daemon-reload"]).await {
                Ok(out) if !out.status.success() => {
                    eprintln!(
                        "infmonctl: install: warning: systemctl daemon-reload failed; \
                               the new unit file may not be recognized until a manual reload"
                    );
                }
                Err(e) => {
                    eprintln!("infmonctl: install: warning: daemon-reload: {e}");
                }
                _ => {}
            }
            tracing::info!("installed systemd unit to {}", service_dst.display());
        }
    } else {
        eprintln!(
            "infmonctl: install: systemd unit file not found; \
             skipping service installation (you can install it manually)"
        );
    }

    eprintln!("infmonctl: install: completed successfully");
    EXIT_SUCCESS
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

    let output = run_cmd("systemctl", &[action, "infmon"]).await;
    match output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("infmonctl: {action}: systemctl failed: {}", stderr.trim());
                return EXIT_FAILURE;
            }
            eprintln!("infmonctl: {action}: OK");
            EXIT_SUCCESS
        }
        Err(e) => {
            eprintln!("infmonctl: {action}: failed to run systemctl: {e}");
            EXIT_FAILURE
        }
    }
}

async fn run_status(cli: &Cli) -> i32 {
    let output = cli.effective_output();

    // Check systemd service status
    let systemd_active = match run_cmd("systemctl", &["is-active", "infmon"]).await {
        Ok(out) => {
            let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
            state == "active"
        }
        Err(_) => false,
    };

    let systemd_state = match run_cmd(
        "systemctl",
        &[
            "show",
            "infmon",
            "--property=ActiveState,SubState",
            "--value",
        ],
    )
    .await
    {
        Ok(out) => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let parts: Vec<&str> = raw.trim().lines().collect();
            if parts.len() == 2 {
                format!("{}/{}", parts[0].trim(), parts[1].trim())
            } else {
                raw.trim().to_string()
            }
        }
        Err(_) => "unknown".to_string(),
    };

    // Check if the frontend socket is responsive
    let client = make_client(cli);
    let frontend_responsive = client.stats_show(None).await.is_ok();

    let status = ServiceStatus {
        systemd_active,
        systemd_state: systemd_state.clone(),
        frontend_responsive,
    };

    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "systemd_active": status.systemd_active,
                    "systemd_state": status.systemd_state,
                    "frontend_responsive": status.frontend_responsive,
                })
            );
        }
        OutputFormat::Table => {
            let service_icon = if status.systemd_active { "●" } else { "○" };
            let frontend_icon = if status.frontend_responsive {
                "●"
            } else {
                "○"
            };
            println!("{} systemd: {}", service_icon, status.systemd_state);
            println!(
                "{} frontend: {}",
                frontend_icon,
                if status.frontend_responsive {
                    "responsive"
                } else {
                    "not responding"
                }
            );
        }
    }

    if status.systemd_active && status.frontend_responsive {
        EXIT_SUCCESS
    } else if status.systemd_active {
        // Service running but frontend not responsive
        EXIT_DEGRADED
    } else {
        EXIT_SERVICE_UNAVAILABLE
    }
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
    let client = make_client(cli);
    let output = cli.effective_output();

    match cmd {
        FlowRuleCommands::Add { ref spec } => {
            if !is_root() {
                eprintln!("infmonctl: flow-rule add requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }

            // Parse spec: name=<name> fields=<f1,f2,...> max_keys=<N>
            let mut name = None;
            let mut fields = Vec::new();
            let mut max_keys = 1024u32;
            let mut eviction_policy = infmon_common::config::model::EvictionPolicy::LruDrop;

            for kv in spec {
                if let Some((k, v)) = kv.split_once('=') {
                    match k {
                        "name" => name = Some(v.to_string()),
                        "fields" => {
                            for f in v.split(',') {
                                match f.trim() {
                                    "src_ip" => {
                                        fields.push(infmon_common::config::model::Field::SrcIp)
                                    }
                                    "dst_ip" => {
                                        fields.push(infmon_common::config::model::Field::DstIp)
                                    }
                                    "ip_proto" => {
                                        fields.push(infmon_common::config::model::Field::IpProto)
                                    }
                                    "dscp" => {
                                        fields.push(infmon_common::config::model::Field::Dscp)
                                    }
                                    "mirror_src_ip" => fields
                                        .push(infmon_common::config::model::Field::MirrorSrcIp),
                                    other => {
                                        eprintln!("infmonctl: unknown field: {other}");
                                        return EXIT_USAGE;
                                    }
                                }
                            }
                        }
                        "max_keys" => {
                            max_keys = match v.parse() {
                                Ok(n) => n,
                                Err(_) => {
                                    eprintln!("infmonctl: invalid max_keys: {v}");
                                    return EXIT_USAGE;
                                }
                            };
                        }
                        "eviction_policy" => {
                            eviction_policy = match v {
                                "lru_drop" => infmon_common::config::model::EvictionPolicy::LruDrop,
                                other => {
                                    eprintln!("infmonctl: unknown eviction_policy: {other} (supported: lru_drop)");
                                    return EXIT_USAGE;
                                }
                            };
                        }
                        _ => {
                            eprintln!("infmonctl: unknown spec key: {k}");
                            return EXIT_USAGE;
                        }
                    }
                } else {
                    eprintln!("infmonctl: invalid spec format, expected key=value: {kv}");
                    return EXIT_USAGE;
                }
            }

            let name = match name {
                Some(n) => n,
                None => {
                    eprintln!("infmonctl: flow-rule add: name=<name> is required");
                    return EXIT_USAGE;
                }
            };

            if fields.is_empty() {
                eprintln!("infmonctl: flow-rule add: fields=<field,...> is required");
                return EXIT_USAGE;
            }

            let def = infmon_common::ipc::control_client::FlowRuleDef {
                name: name.clone(),
                fields,
                max_keys,
                eviction_policy,
            };

            match client.flow_rule_add(def).await {
                Ok(id) => {
                    match output {
                        OutputFormat::Json => {
                            println!(
                                "{}",
                                serde_json::json!({"id": id.to_string(), "name": name})
                            );
                        }
                        OutputFormat::Table => {
                            println!("Added flow rule '{}' (id: {})", name, id);
                        }
                    }
                    EXIT_SUCCESS
                }
                Err(e) => {
                    eprintln!("infmonctl: flow-rule add: {e}");
                    ctl_error_to_exit_code(&e)
                }
            }
        }
        FlowRuleCommands::Rm { ref target, all } => {
            if !is_root() {
                eprintln!("infmonctl: flow-rule rm requires root privileges");
                return EXIT_PERMISSION_DENIED;
            }
            if *all {
                eprintln!("infmonctl: flow-rule rm --all: not yet implemented");
                return EXIT_FAILURE;
            }

            match client.flow_rule_rm(target).await {
                Ok(()) => {
                    if !cli.quiet {
                        eprintln!("Removed flow rule '{target}'");
                    }
                    EXIT_SUCCESS
                }
                Err(e) => {
                    eprintln!("infmonctl: flow-rule rm: {e}");
                    ctl_error_to_exit_code(&e)
                }
            }
        }
        FlowRuleCommands::List => match client.flow_rule_list().await {
            Ok(rules) => {
                let data: Vec<FlowRuleData> = rules
                    .iter()
                    .map(|r| FlowRuleData {
                        name: r.name.clone(),
                        fields: r.fields.clone(),
                        max_keys: r.max_keys,
                        eviction_policy: r.eviction_policy,
                    })
                    .collect();
                print_output(&FlowRuleListOutput(data), &output, cli.compact);
                EXIT_SUCCESS
            }
            Err(e) => {
                eprintln!("infmonctl: flow-rule list: {e}");
                ctl_error_to_exit_code(&e)
            }
        },
        FlowRuleCommands::Show { ref target } => match client.flow_rule_show(target).await {
            Ok(stats) => {
                let detail = FlowRuleDetailData {
                    name: stats.name,
                    fields: stats.fields.iter().map(field_id_to_field).collect(),
                    max_keys: stats.max_keys,
                    eviction_policy: stats.eviction_policy,
                    counters: infmon_common::ipc::protocol::FlowRuleCountersData {
                        packets: stats.counters.packets,
                        bytes: stats.counters.bytes,
                        evictions: stats.counters.evictions,
                        drops: stats.counters.drops,
                    },
                    flows: stats
                        .flows
                        .iter()
                        .map(|f| infmon_common::ipc::protocol::FlowEntryData {
                            key: f.key.iter().map(|v| format!("{:?}", v)).collect(),
                            packets: f.counters.packets,
                            bytes: f.counters.bytes,
                            first_seen_ns: f.counters.first_seen_ns,
                            last_seen_ns: f.counters.last_seen_ns,
                        })
                        .collect(),
                };
                print_output(&FlowRuleShowOutput(detail), &output, cli.compact);
                EXIT_SUCCESS
            }
            Err(e) => {
                eprintln!("infmonctl: flow-rule show: {e}");
                ctl_error_to_exit_code(&e)
            }
        },
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
    let client = make_client(cli);
    let output = cli.effective_output();

    match cmd {
        StatsCommands::Show {
            ref name,
            top,
            ref watch,
        } => {
            if *top > 0 {
                eprintln!("infmonctl: stats show --top: not yet implemented, showing all");
            }
            if watch.is_some() {
                eprintln!("infmonctl: stats show --watch: not yet implemented, showing once");
            }

            match client.stats_show(name.as_deref()).await {
                Ok(data) => {
                    print_output(&StatsShowOutput(data), &output, cli.compact);
                    EXIT_SUCCESS
                }
                Err(e) => {
                    eprintln!("infmonctl: stats show: {e}");
                    ctl_error_to_exit_code(&e)
                }
            }
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
// Output types with TableDisplay + Serialize
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct FlowRuleListOutput(Vec<FlowRuleData>);

impl TableDisplay for FlowRuleListOutput {
    fn print_table(&self) {
        if self.0.is_empty() {
            println!("No flow rules configured.");
            return;
        }
        println!("{:<20} {:<30} {:>10}", "NAME", "FIELDS", "MAX_KEYS");
        for r in &self.0 {
            let fields: Vec<String> = r
                .fields
                .iter()
                .map(|f| format!("{:?}", f).to_lowercase())
                .collect();
            println!("{:<20} {:<30} {:>10}", r.name, fields.join(","), r.max_keys);
        }
    }
}

#[derive(serde::Serialize)]
struct FlowRuleShowOutput(FlowRuleDetailData);

impl TableDisplay for FlowRuleShowOutput {
    fn print_table(&self) {
        let d = &self.0;
        println!("Name:       {}", d.name);
        let fields: Vec<String> = d
            .fields
            .iter()
            .map(|f| format!("{:?}", f).to_lowercase())
            .collect();
        println!("Fields:     {}", fields.join(", "));
        println!("Max keys:   {}", d.max_keys);
        println!("Counters:");
        println!("  Packets:    {}", d.counters.packets);
        println!("  Bytes:      {}", d.counters.bytes);
        println!("  Evictions:  {}", d.counters.evictions);
        println!("  Drops:      {}", d.counters.drops);
        if !d.flows.is_empty() {
            println!("Flows ({}):", d.flows.len());
            for f in &d.flows {
                println!(
                    "  Key: {}  Pkts: {}  Bytes: {}",
                    f.key.join(","),
                    f.packets,
                    f.bytes
                );
            }
        }
    }
}

#[derive(serde::Serialize)]
struct StatsShowOutput(StatsShowData);

impl TableDisplay for StatsShowOutput {
    fn print_table(&self) {
        if self.0.flow_rules.is_empty() {
            println!("No flow rule stats available.");
            return;
        }
        println!(
            "{:<20} {:>12} {:>12} {:>10} {:>8} {:>8}",
            "RULE", "PACKETS", "BYTES", "FLOWS", "EVICT", "DROPS"
        );
        for r in &self.0.flow_rules {
            println!(
                "{:<20} {:>12} {:>12} {:>10} {:>8} {:>8}",
                r.name, r.packets, r.bytes, r.active_flows, r.evictions, r.drops
            );
        }
    }
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

fn ctl_error_to_exit_code(e: &infmon_common::ipc::CtlError) -> i32 {
    use infmon_common::ipc::CtlError;
    match e {
        CtlError::Connect(_) => EXIT_FRONTEND_UNREACHABLE,
        CtlError::Backend { code, .. } => match *code {
            3 => EXIT_NOT_FOUND,
            6 => EXIT_CONFLICT,
            _ => EXIT_FAILURE,
        },
        _ => EXIT_FAILURE,
    }
}

fn field_id_to_field(
    id: &infmon_common::ipc::types::FieldId,
) -> infmon_common::config::model::Field {
    use infmon_common::config::model::Field;
    use infmon_common::ipc::types::FieldId;
    match id {
        FieldId::SrcIp => Field::SrcIp,
        FieldId::DstIp => Field::DstIp,
        FieldId::IpProto => Field::IpProto,
        FieldId::Dscp => Field::Dscp,
        FieldId::MirrorSrcIp => Field::MirrorSrcIp,
    }
}

// ---------------------------------------------------------------------------
// ServiceStatus for run_status
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ServiceStatus {
    systemd_active: bool,
    systemd_state: String,
    frontend_responsive: bool,
}

// ---------------------------------------------------------------------------
// Shell command helpers for lifecycle management
// ---------------------------------------------------------------------------

/// Run an external command and return its output.
async fn run_cmd(program: &str, args: &[&str]) -> Result<std::process::Output, std::io::Error> {
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || std::process::Command::new(&program).args(&args).output())
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// Ensure the `infmon` system user and group exist (idempotent).
async fn ensure_system_user() -> Result<(), i32> {
    // Check if user already exists
    match run_cmd("id", &["infmon"]).await {
        Ok(out) if out.status.success() => return Ok(()),
        _ => {}
    }

    // Create system user
    let result = run_cmd(
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "infmon",
        ],
    )
    .await;

    match result {
        Ok(out) if out.status.success() => {
            tracing::info!("created system user 'infmon'");
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "infmonctl: install: failed to create user 'infmon': {}",
                stderr.trim()
            );
            Err(EXIT_FAILURE)
        }
        Err(e) => {
            eprintln!("infmonctl: install: failed to run useradd: {e}");
            Err(EXIT_FAILURE)
        }
    }
}
