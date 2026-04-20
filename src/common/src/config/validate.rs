use std::collections::HashSet;

use crate::config::model::{Config, ExporterEntry, Field, FlowRule, FrontendConfig};
use thiserror::Error;

pub const MAX_KEY_WIDTH: u32 = 64;
pub const MAX_KEYS_BUDGET: u32 = 16 * 1024 * 1024;
pub const NAME_MAX_LEN: usize = 31;
pub const NAME_MIN_LEN: usize = 2;

/// Known exporter type keys. Unknown types are a config error per spec §5.
pub const KNOWN_EXPORTER_TYPES: &[&str] = &["otlp"];

/// Valid overflow policies. v1 only supports `drop_newest`.
pub const VALID_OVERFLOW_POLICIES: &[&str] = &["drop_newest"];

/// Maximum allowed queue_depth per exporter.
pub const MAX_QUEUE_DEPTH: usize = 10_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("duplicate flow-rule name: {0}")]
    DuplicateName(String),
    #[error("flow-rule '{name}': fields list is empty")]
    EmptyFields { name: String },
    #[error("flow-rule '{name}': duplicate field {field:?}")]
    DuplicateField { name: String, field: Field },
    #[error("flow-rule '{name}': max_keys must be >= 1")]
    ZeroMaxKeys { name: String },
    #[error("flow-rule '{name}': max_keys {max_keys} exceeds per-rule budget {MAX_KEYS_BUDGET}")]
    MaxKeysExceedsBudget { name: String, max_keys: u32 },
    #[error("flow-rule '{name}': key width {width} exceeds maximum {MAX_KEY_WIDTH}")]
    KeyWidthExceeded { name: String, width: u32 },
    #[error("total max_keys {total} exceeds budget {MAX_KEYS_BUDGET}")]
    BudgetExceeded { total: u64 },
    #[error("too many flow rules: {count} exceeds maximum {max}")]
    TooManyRules { count: usize, max: usize },
    #[error("flow-rule name '{name}' is invalid: must match ^[a-z0-9][a-z0-9_-]{{1,30}}$")]
    InvalidName { name: String },
    #[error("frontend: polling_interval_ms must be > 0")]
    ZeroPollingInterval,
    #[error("frontend: shutdown_grace_ms must be > 0")]
    ZeroShutdownGrace,
    #[error("frontend: invalid startup_timeout format: {value:?}")]
    InvalidStartupTimeout { value: String },
    #[error("exporter '{name}': type is empty")]
    EmptyExporterType { name: String },
    #[error("exporter '{name}': unknown type '{kind}' (known: {known})")]
    UnknownExporterType {
        name: String,
        kind: String,
        known: String,
    },
    #[error("exporter at index {index}: name is empty")]
    EmptyExporterName { index: usize },
    #[error("duplicate exporter name: {0}")]
    DuplicateExporterName(String),
    #[error("exporter '{name}': queue_depth must be >= 1")]
    ZeroQueueDepth { name: String },
    #[error("exporter '{name}': queue_depth {depth} exceeds maximum {max}")]
    QueueDepthTooLarge {
        name: String,
        depth: usize,
        max: usize,
    },
    #[error("exporter '{name}': invalid export_timeout format: {value:?}")]
    InvalidExportTimeout { name: String, value: String },
    #[error("exporter '{name}': unknown on_overflow policy '{policy}' (valid: {valid})")]
    InvalidOverflowPolicy {
        name: String,
        policy: String,
        valid: String,
    },
    #[error("frontend: startup_timeout must be > 0")]
    ZeroStartupTimeout,
    #[error("exporter '{name}': export_timeout must be > 0")]
    ZeroExportTimeout { name: String },
    #[error("exporter '{name}': name is invalid: must match ^[a-z0-9][a-z0-9_-]{{1,30}}$")]
    InvalidExporterName { name: String },
    #[error("exporter '{name}': OTLP exporter must have a non-empty 'endpoint'")]
    MissingOtlpEndpoint { name: String },
}

/// Validate a flow-rule name.
///
/// Names must match `^[a-z0-9][a-z0-9_-]{1,30}$` — i.e. 2–31 chars,
/// starting with a lowercase letter **or digit**, followed by lowercase
/// alphanumerics, hyphens, or underscores. Leading digits are intentionally
/// allowed since flow-rule names are never used as programming-language
/// identifiers.
fn is_valid_name(name: &str) -> bool {
    let len = name.len();
    if !(NAME_MIN_LEN..=NAME_MAX_LEN).contains(&len) {
        return false;
    }
    let bytes = name.as_bytes();
    // First char: [a-z0-9]
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    // Rest: [a-z0-9_-]
    bytes[1..]
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Parse a duration string like `"800ms"`, `"5s"`, `"2m"`, `"1h"`.
///
/// Supported suffixes: `ms`, `s`, `m`, `h`. Bare integers are treated as
/// milliseconds. Returns `None` for unrecognised formats.
fn parse_duration_str(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<u64>().ok()
    } else if let Some(h) = s.strip_suffix('h') {
        h.trim().parse::<u64>().ok().map(|v| v * 3_600_000)
    } else if let Some(m) = s.strip_suffix('m') {
        m.trim().parse::<u64>().ok().map(|v| v * 60_000)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.trim().parse::<u64>().ok().map(|v| v * 1000)
    } else if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        s.parse::<u64>().ok()
    } else {
        None
    }
}

/// Validate a single flow-rule in isolation
pub fn validate_rule(rule: &FlowRule) -> Result<(), ValidationError> {
    if !is_valid_name(&rule.name) {
        return Err(ValidationError::InvalidName {
            name: rule.name.clone(),
        });
    }
    if rule.fields.is_empty() {
        return Err(ValidationError::EmptyFields {
            name: rule.name.clone(),
        });
    }
    let mut seen = HashSet::new();
    for &field in &rule.fields {
        if !seen.insert(field) {
            return Err(ValidationError::DuplicateField {
                name: rule.name.clone(),
                field,
            });
        }
    }
    if rule.max_keys == 0 {
        return Err(ValidationError::ZeroMaxKeys {
            name: rule.name.clone(),
        });
    }
    if rule.max_keys > MAX_KEYS_BUDGET {
        return Err(ValidationError::MaxKeysExceedsBudget {
            name: rule.name.clone(),
            max_keys: rule.max_keys,
        });
    }
    let width: u32 = rule.fields.iter().map(|f| f.width()).sum();
    if width > MAX_KEY_WIDTH {
        return Err(ValidationError::KeyWidthExceeded {
            name: rule.name.clone(),
            width,
        });
    }
    Ok(())
}

/// Validate the frontend configuration block.
pub fn validate_frontend(cfg: &FrontendConfig) -> Result<(), ValidationError> {
    if cfg.polling_interval_ms == 0 {
        return Err(ValidationError::ZeroPollingInterval);
    }
    if cfg.shutdown_grace_ms == 0 {
        return Err(ValidationError::ZeroShutdownGrace);
    }
    match parse_duration_str(&cfg.startup_timeout) {
        None => {
            return Err(ValidationError::InvalidStartupTimeout {
                value: cfg.startup_timeout.clone(),
            });
        }
        Some(0) => {
            return Err(ValidationError::ZeroStartupTimeout);
        }
        Some(_) => {}
    }
    Ok(())
}

/// Validate a single exporter entry.
///
/// `index` is used for error messages when the name is empty.
pub fn validate_exporter(entry: &ExporterEntry, index: usize) -> Result<(), ValidationError> {
    if entry.name.is_empty() {
        return Err(ValidationError::EmptyExporterName { index });
    }
    if !is_valid_name(&entry.name) {
        return Err(ValidationError::InvalidExporterName {
            name: entry.name.clone(),
        });
    }
    if entry.kind.is_empty() {
        return Err(ValidationError::EmptyExporterType {
            name: entry.name.clone(),
        });
    }
    if !KNOWN_EXPORTER_TYPES.contains(&entry.kind.as_str()) {
        return Err(ValidationError::UnknownExporterType {
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            known: KNOWN_EXPORTER_TYPES.join(", "),
        });
    }
    if entry.queue_depth == 0 {
        return Err(ValidationError::ZeroQueueDepth {
            name: entry.name.clone(),
        });
    }
    if entry.queue_depth > MAX_QUEUE_DEPTH {
        return Err(ValidationError::QueueDepthTooLarge {
            name: entry.name.clone(),
            depth: entry.queue_depth,
            max: MAX_QUEUE_DEPTH,
        });
    }
    match parse_duration_str(&entry.export_timeout) {
        None => {
            return Err(ValidationError::InvalidExportTimeout {
                name: entry.name.clone(),
                value: entry.export_timeout.clone(),
            });
        }
        Some(0) => {
            return Err(ValidationError::ZeroExportTimeout {
                name: entry.name.clone(),
            });
        }
        Some(_) => {}
    }
    // OTLP exporters must have a non-empty endpoint
    if entry.kind == "otlp" {
        let has_endpoint = entry
            .extra
            .get("endpoint")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        if !has_endpoint {
            return Err(ValidationError::MissingOtlpEndpoint {
                name: entry.name.clone(),
            });
        }
    }
    if !VALID_OVERFLOW_POLICIES.contains(&entry.on_overflow.as_str()) {
        return Err(ValidationError::InvalidOverflowPolicy {
            name: entry.name.clone(),
            policy: entry.on_overflow.clone(),
            valid: VALID_OVERFLOW_POLICIES.join(", "),
        });
    }
    Ok(())
}

/// Validate the entire config (all rules, cross-rule constraints,
/// frontend config, and exporter entries).
pub fn validate_config(config: &Config) -> Result<(), ValidationError> {
    // Validate frontend section if present
    if let Some(ref frontend) = config.frontend {
        validate_frontend(frontend)?;
    }

    // Validate flow rules
    let mut names = HashSet::new();
    for rule in &config.flow_rules {
        if !names.insert(&rule.name) {
            return Err(ValidationError::DuplicateName(rule.name.clone()));
        }
        validate_rule(rule)?;
    }
    if config.flow_rules.len() > crate::config::crud::FLOW_RULE_SET_MAX {
        return Err(ValidationError::TooManyRules {
            count: config.flow_rules.len(),
            max: crate::config::crud::FLOW_RULE_SET_MAX,
        });
    }
    let total: u64 = config.flow_rules.iter().map(|r| r.max_keys as u64).sum();
    if total > MAX_KEYS_BUDGET as u64 {
        return Err(ValidationError::BudgetExceeded { total });
    }

    // Validate exporters section if present
    if let Some(ref exporters) = config.exporters {
        let mut exporter_names = HashSet::new();
        for (i, entry) in exporters.iter().enumerate() {
            validate_exporter(entry, i)?;
            if !exporter_names.insert(&entry.name) {
                return Err(ValidationError::DuplicateExporterName(entry.name.clone()));
            }
        }
    }

    Ok(())
}
