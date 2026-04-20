use clap::Parser;
use infmon_cli::{
    Cli, Commands, ConfigCommands, FlowCommands, FlowRuleCommands, LogCommands, StatsCommands,
};

/// Helper: parse CLI args, returning the Cli struct
fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["infmonctl"];
    full.extend_from_slice(args);
    Cli::parse_from(full)
}

/// Helper: try parsing, returning Err on failure
fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
    let mut full = vec!["infmonctl"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full)
}

// ---- Top-level verbs ----

#[test]
fn parse_install() {
    let cli = parse(&["install"]);
    assert!(matches!(cli.command, Commands::Install { force: false }));
}

#[test]
fn parse_install_force() {
    let cli = parse(&["install", "--force"]);
    assert!(matches!(cli.command, Commands::Install { force: true }));
}

#[test]
fn parse_uninstall() {
    let cli = parse(&["uninstall"]);
    assert!(matches!(cli.command, Commands::Uninstall { purge: false }));
}

#[test]
fn parse_uninstall_purge() {
    let cli = parse(&["uninstall", "--purge"]);
    assert!(matches!(cli.command, Commands::Uninstall { purge: true }));
}

#[test]
fn parse_start() {
    let cli = parse(&["start"]);
    assert!(matches!(cli.command, Commands::Start));
}

#[test]
fn parse_stop() {
    let cli = parse(&["stop"]);
    assert!(matches!(cli.command, Commands::Stop));
}

#[test]
fn parse_restart() {
    let cli = parse(&["restart"]);
    assert!(matches!(cli.command, Commands::Restart));
}

#[test]
fn parse_status() {
    let cli = parse(&["status"]);
    assert!(matches!(cli.command, Commands::Status));
}

#[test]
fn parse_health() {
    let cli = parse(&["health"]);
    assert!(matches!(cli.command, Commands::Health));
}

// ---- Config subcommands ----

#[test]
fn parse_config_get() {
    let cli = parse(&["config", "get", "exporter.otlp.endpoint"]);
    match cli.command {
        Commands::Config {
            command: ConfigCommands::Get { key },
        } => {
            assert_eq!(key, "exporter.otlp.endpoint");
        }
        _ => panic!("expected config get"),
    }
}

#[test]
fn parse_config_set() {
    let cli = parse(&["config", "set", "frontend.polling_interval_ms", "500"]);
    match cli.command {
        Commands::Config {
            command: ConfigCommands::Set { key, value, r#type },
        } => {
            assert_eq!(key, "frontend.polling_interval_ms");
            assert_eq!(value, "500");
            assert!(r#type.is_none());
        }
        _ => panic!("expected config set"),
    }
}

#[test]
fn parse_config_set_with_type() {
    let cli = parse(&["config", "set", "key", "val", "--type", "int"]);
    match cli.command {
        Commands::Config {
            command: ConfigCommands::Set { r#type, .. },
        } => {
            assert_eq!(r#type, Some("int".to_string()));
        }
        _ => panic!("expected config set"),
    }
}

#[test]
fn parse_config_reload() {
    let cli = parse(&["config", "reload"]);
    assert!(matches!(
        cli.command,
        Commands::Config {
            command: ConfigCommands::Reload
        }
    ));
}

#[test]
fn parse_config_show() {
    let cli = parse(&["config", "show"]);
    match cli.command {
        Commands::Config {
            command: ConfigCommands::Show { annotate },
        } => {
            assert!(!annotate);
        }
        _ => panic!("expected config show"),
    }
}

#[test]
fn parse_config_show_annotate() {
    let cli = parse(&["config", "show", "--annotate"]);
    match cli.command {
        Commands::Config {
            command: ConfigCommands::Show { annotate },
        } => {
            assert!(annotate);
        }
        _ => panic!("expected config show --annotate"),
    }
}

// ---- Flow-rule subcommands ----

#[test]
fn parse_flow_rule_add() {
    let cli = parse(&["flow-rule", "add", "src_ip=10.0.0.0/8", "name=test"]);
    match cli.command {
        Commands::FlowRule {
            command: FlowRuleCommands::Add { spec },
        } => {
            assert_eq!(spec, vec!["src_ip=10.0.0.0/8", "name=test"]);
        }
        _ => panic!("expected flow-rule add"),
    }
}

#[test]
fn parse_flow_rule_rm() {
    let cli = parse(&["flow-rule", "rm", "test-rule"]);
    match cli.command {
        Commands::FlowRule {
            command: FlowRuleCommands::Rm { target, all },
        } => {
            assert_eq!(target, "test-rule");
            assert!(!all);
        }
        _ => panic!("expected flow-rule rm"),
    }
}

#[test]
fn parse_flow_rule_rm_all() {
    let cli = parse(&["flow-rule", "rm", "test-rule", "--all"]);
    match cli.command {
        Commands::FlowRule {
            command: FlowRuleCommands::Rm { target, all },
        } => {
            assert_eq!(target, "test-rule");
            assert!(all);
        }
        _ => panic!("expected flow-rule rm --all"),
    }
}

#[test]
fn parse_flow_rule_list() {
    let cli = parse(&["flow-rule", "list"]);
    assert!(matches!(
        cli.command,
        Commands::FlowRule {
            command: FlowRuleCommands::List
        }
    ));
}

#[test]
fn parse_flow_rule_show() {
    let cli = parse(&["flow-rule", "show", "my-rule"]);
    match cli.command {
        Commands::FlowRule {
            command: FlowRuleCommands::Show { target },
        } => {
            assert_eq!(target, "my-rule");
        }
        _ => panic!("expected flow-rule show"),
    }
}

// ---- Flow subcommands ----

#[test]
fn parse_flow_list() {
    let cli = parse(&["flow", "list", "my-rule"]);
    match cli.command {
        Commands::Flow {
            command: FlowCommands::List { rule, top, .. },
        } => {
            assert_eq!(rule, "my-rule");
            assert_eq!(top, 50);
        }
        _ => panic!("expected flow list"),
    }
}

#[test]
fn parse_flow_list_top_sort() {
    let cli = parse(&["flow", "list", "rule1", "--top", "10", "--sort", "packets"]);
    match cli.command {
        Commands::Flow {
            command: FlowCommands::List { rule, top, .. },
        } => {
            assert_eq!(rule, "rule1");
            assert_eq!(top, 10);
        }
        _ => panic!("expected flow list"),
    }
}

#[test]
fn parse_flow_show() {
    let cli = parse(&["flow", "show", "rule1", "src_ip=10.0.0.7 dst_ip=10.0.1.4"]);
    match cli.command {
        Commands::Flow {
            command: FlowCommands::Show { rule, key },
        } => {
            assert_eq!(rule, "rule1");
            assert_eq!(key, "src_ip=10.0.0.7 dst_ip=10.0.1.4");
        }
        _ => panic!("expected flow show"),
    }
}

// ---- Stats subcommands ----

#[test]
fn parse_stats_show() {
    let cli = parse(&["stats", "show"]);
    match cli.command {
        Commands::Stats {
            command: StatsCommands::Show { name, top, watch },
        } => {
            assert!(name.is_none());
            assert_eq!(top, 20);
            assert!(watch.is_none());
        }
        _ => panic!("expected stats show"),
    }
}

#[test]
fn parse_stats_show_with_options() {
    let cli = parse(&[
        "stats", "show", "--name", "export_*", "--top", "5", "--watch", "2",
    ]);
    match cli.command {
        Commands::Stats {
            command: StatsCommands::Show { name, top, watch },
        } => {
            assert_eq!(name, Some("export_*".to_string()));
            assert_eq!(top, 5);
            assert_eq!(watch, Some(2));
        }
        _ => panic!("expected stats show"),
    }
}

#[test]
fn parse_stats_export() {
    let cli = parse(&["stats", "export", "--format", "prom"]);
    assert!(matches!(
        cli.command,
        Commands::Stats {
            command: StatsCommands::Export { .. }
        }
    ));
}

// ---- Log subcommands ----

#[test]
fn parse_log_tail() {
    let cli = parse(&["log", "tail"]);
    match cli.command {
        Commands::Log {
            command: LogCommands::Tail { follow, since, n },
        } => {
            assert!(!follow);
            assert!(since.is_none());
            assert_eq!(n, 100);
        }
        _ => panic!("expected log tail"),
    }
}

#[test]
fn parse_log_tail_follow() {
    let cli = parse(&["log", "tail", "-f", "--since", "10 min ago", "-n", "50"]);
    match cli.command {
        Commands::Log {
            command: LogCommands::Tail { follow, since, n },
        } => {
            assert!(follow);
            assert_eq!(since, Some("10 min ago".to_string()));
            assert_eq!(n, 50);
        }
        _ => panic!("expected log tail"),
    }
}

// ---- Global flags ----

#[test]
fn parse_json_flag() {
    let cli = parse(&["--json", "status"]);
    assert!(cli.json);
    assert!(matches!(
        cli.effective_output(),
        infmon_cli::OutputFormat::Json
    ));
}

#[test]
fn parse_output_json() {
    let cli = parse(&["--output", "json", "status"]);
    assert!(matches!(
        cli.effective_output(),
        infmon_cli::OutputFormat::Json
    ));
}

#[test]
fn parse_global_flags() {
    let cli = parse(&[
        "--compact",
        "--raw-bytes",
        "--no-color",
        "-q",
        "-vvv",
        "--config",
        "/tmp/test.yaml",
        "--socket",
        "/tmp/test.sock",
        "--timeout",
        "10",
        "health",
    ]);
    assert!(cli.compact);
    assert!(cli.raw_bytes);
    assert!(cli.no_color);
    assert!(cli.quiet);
    assert_eq!(cli.verbose, 3);
    assert_eq!(cli.config, "/tmp/test.yaml");
    assert_eq!(cli.socket, "/tmp/test.sock");
    assert_eq!(cli.timeout, 10);
}

// ---- Rejection cases ----

#[test]
fn reject_no_subcommand() {
    assert!(try_parse(&[]).is_err());
}

#[test]
fn reject_unknown_subcommand() {
    assert!(try_parse(&["foobar"]).is_err());
}

#[test]
fn reject_stats_export_without_format() {
    assert!(try_parse(&["stats", "export"]).is_err());
}

#[test]
fn reject_flow_rule_rm_without_target() {
    assert!(try_parse(&["flow-rule", "rm"]).is_err());
}

#[test]
fn reject_config_get_without_key() {
    assert!(try_parse(&["config", "get"]).is_err());
}

#[test]
fn reject_config_set_without_value() {
    assert!(try_parse(&["config", "set", "key"]).is_err());
}

#[test]
fn reject_flow_show_without_key() {
    assert!(try_parse(&["flow", "show", "rule1"]).is_err());
}

#[test]
fn reject_flow_rule_add_without_spec() {
    assert!(try_parse(&["flow-rule", "add"]).is_err());
}
