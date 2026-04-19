use std::collections::HashSet;

use crate::model::{Config, Field, FlowRule};
use thiserror::Error;

pub const MAX_KEY_WIDTH: u32 = 64;
pub const MAX_KEYS_BUDGET: u32 = 16 * 1024 * 1024;
pub const NAME_MAX_LEN: usize = 31;
pub const NAME_MIN_LEN: usize = 2;

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
    #[error("flow-rule name '{name}' is invalid: must match ^[a-z0-9][a-z0-9_-]{{1,30}}$")]
    InvalidName { name: String },
}

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

/// Validate the entire config (all rules, cross-rule constraints)
pub fn validate_config(config: &Config) -> Result<(), ValidationError> {
    let mut names = HashSet::new();
    for rule in &config.flow_rules {
        validate_rule(rule)?;
        if !names.insert(&rule.name) {
            return Err(ValidationError::DuplicateName(rule.name.clone()));
        }
    }
    let total: u64 = config.flow_rules.iter().map(|r| r.max_keys as u64).sum();
    if total > MAX_KEYS_BUDGET as u64 {
        return Err(ValidationError::BudgetExceeded { total });
    }
    Ok(())
}
