use super::*;

#[test]
fn parse_duration_ms() {
    assert_eq!(parse_duration("800ms"), Some(Duration::from_millis(800)));
}

#[test]
fn parse_duration_s() {
    assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
}

#[test]
fn parse_duration_m() {
    assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
}

#[test]
fn parse_duration_h() {
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
}

#[test]
fn parse_duration_bare_number() {
    assert_eq!(parse_duration("1000"), Some(Duration::from_millis(1000)));
}

#[test]
fn parse_duration_invalid() {
    assert_eq!(parse_duration("abc"), None);
}

#[test]
fn entry_to_config_basic() {
    let entry = infmon_common::config::model::ExporterEntry {
        kind: "otlp".into(),
        name: "primary".into(),
        queue_depth: 2,
        export_timeout: "800ms".into(),
        on_overflow: "drop_newest".into(),
        extra: std::collections::HashMap::new(),
    };
    let cfg = entry_to_exporter_config(&entry);
    assert_eq!(cfg.kind, "otlp");
    assert_eq!(cfg.name, "primary");
    assert_eq!(cfg.queue_depth, 2);
    assert_eq!(cfg.export_timeout, Duration::from_millis(800));
}

// ── parse_duration edge cases ───────────────────────────────────

#[test]
fn parse_duration_zero_ms() {
    assert_eq!(parse_duration("0ms"), Some(Duration::from_millis(0)));
}

#[test]
fn parse_duration_zero_bare() {
    assert_eq!(parse_duration("0"), Some(Duration::from_millis(0)));
}

#[test]
fn parse_duration_whitespace_trimmed() {
    assert_eq!(parse_duration("  5s  "), Some(Duration::from_secs(5)));
}

#[test]
fn parse_duration_large_value() {
    assert_eq!(parse_duration("86400s"), Some(Duration::from_secs(86400)));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration("24h"), Some(Duration::from_secs(24 * 3600)));
}

#[test]
fn parse_duration_empty_string() {
    // Empty string after trim has no digits, treated as invalid
    assert_eq!(parse_duration(""), None);
}

#[test]
fn parse_duration_negative_is_invalid() {
    assert_eq!(parse_duration("-5s"), None);
}

#[test]
fn parse_duration_float_is_invalid() {
    assert_eq!(parse_duration("1.5s"), None);
}

#[test]
fn parse_duration_unknown_suffix() {
    assert_eq!(parse_duration("100x"), None);
}

// ── entry_to_exporter_config edge cases ─────────────────────────

#[test]
fn entry_to_config_with_extra_fields() {
    let mut extra = std::collections::HashMap::new();
    extra.insert(
        "endpoint".to_string(),
        serde_yaml::Value::String("http://localhost:4317".into()),
    );
    extra.insert(
        "timeout".to_string(),
        serde_yaml::Value::Number(serde_yaml::Number::from(30)),
    );

    let entry = infmon_common::config::model::ExporterEntry {
        kind: "otlp".into(),
        name: "with-extras".into(),
        queue_depth: 4,
        export_timeout: "5s".into(),
        on_overflow: "drop_newest".into(),
        extra,
    };
    let cfg = entry_to_exporter_config(&entry);
    assert_eq!(cfg.kind, "otlp");
    assert_eq!(cfg.name, "with-extras");
    assert_eq!(cfg.queue_depth, 4);
    assert_eq!(cfg.export_timeout, Duration::from_secs(5));
    assert_eq!(cfg.extra.get("endpoint").unwrap(), "http://localhost:4317");
    assert_eq!(cfg.extra.get("timeout").unwrap(), "30");
}

#[test]
fn entry_to_config_invalid_timeout_uses_default() {
    let entry = infmon_common::config::model::ExporterEntry {
        kind: "test".into(),
        name: "bad-timeout".into(),
        queue_depth: 2,
        export_timeout: "invalid".into(),
        on_overflow: "drop_newest".into(),
        extra: std::collections::HashMap::new(),
    };
    let cfg = entry_to_exporter_config(&entry);
    // Should fallback to 800ms
    assert_eq!(cfg.export_timeout, Duration::from_millis(800));
}

// ── LifecycleError Display ──────────────────────────────────────

#[test]
fn lifecycle_error_display() {
    assert_eq!(
        format!("{}", LifecycleError::ConfigError("bad yaml".into())),
        "config error: bad yaml"
    );
    assert_eq!(
        format!("{}", LifecycleError::BackendUnreachable("timeout".into())),
        "backend unreachable: timeout"
    );
    assert_eq!(
        format!("{}", LifecycleError::UnknownExporter("foo".into())),
        "unknown exporter type: foo"
    );
    assert_eq!(
        format!("{}", LifecycleError::ExporterInit("fail".into())),
        "exporter init failed: fail"
    );
    assert_eq!(
        format!("{}", LifecycleError::ReloadFailed("oops".into())),
        "reload failed: oops"
    );
}

// ── Frontend::start error paths ─────────────────────────────────

#[test]
fn start_fails_on_missing_config() {
    let shutdown = Arc::new(AtomicBool::new(false));
    let result = Frontend::start(
        Path::new("/tmp/nonexistent-infmon-test-config.yaml"),
        shutdown,
    );
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(matches!(err, LifecycleError::ConfigError(_)));
}

#[test]
fn start_fails_on_invalid_yaml() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("bad.yaml");
    std::fs::write(&cfg_path, "{{{{not yaml").unwrap();

    let shutdown = Arc::new(AtomicBool::new(false));
    let result = Frontend::start(&cfg_path, shutdown);
    assert!(result.is_err());
    assert!(matches!(
        result.err().unwrap(),
        LifecycleError::ConfigError(_)
    ));
}

#[test]
fn start_fails_on_unreachable_backend() {
    // Valid YAML with a non-existent stats socket path
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.yaml");
    let yaml = r#"
frontend:
  vpp_stats_socket: /tmp/nonexistent-infmon-test-stats.sock
  startup_timeout: "100ms"
  polling_interval_ms: 100
flow-rules: []
"#;
    std::fs::write(&cfg_path, yaml).unwrap();

    let shutdown = Arc::new(AtomicBool::new(false));
    let result = Frontend::start(&cfg_path, shutdown);
    assert!(result.is_err());
    assert!(matches!(
        result.err().unwrap(),
        LifecycleError::BackendUnreachable(_)
    ));
}

#[test]
fn reload_failed_error_contains_reason() {
    // Verifies that ReloadFailed's Display output includes the underlying message.
    let err = LifecycleError::ReloadFailed("cannot read config: No such file".into());
    assert!(format!("{err}").contains("cannot read config"));
}
