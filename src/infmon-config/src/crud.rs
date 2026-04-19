use crate::model::{Config, FlowRule};
use crate::validate::{validate_config, validate_rule, ValidationError, MAX_KEYS_BUDGET};
use thiserror::Error;

pub const FLOW_RULE_SET_MAX: usize = 16;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CrudError {
    #[error("flow-rule name already exists: {0}")]
    NameExists(String),
    #[error("flow-rule not found: {0}")]
    NotFound(String),
    #[error("validation error: {0}")]
    InvalidSpec(#[from] ValidationError),
    #[error(
        "budget exceeded: adding {requested} keys would exceed budget {budget} (used: {used})"
    )]
    BudgetExceeded {
        used: u64,
        requested: u32,
        budget: u32,
    },
    #[error("flow-rule set is full (max {max})")]
    SetFull { max: usize },
}

pub struct FlowRuleSet {
    rules: Vec<FlowRule>,
    max_keys_budget: u32,
}

impl FlowRuleSet {
    pub fn new(max_keys_budget: u32) -> Self {
        Self {
            rules: Vec::new(),
            max_keys_budget,
        }
    }

    /// Load from Config, validating everything. All-or-nothing.
    pub fn from_config(config: &Config) -> Result<Self, CrudError> {
        validate_config(config)?;
        Ok(Self {
            rules: config.flow_rules.clone(),
            max_keys_budget: MAX_KEYS_BUDGET,
        })
    }

    pub fn add(&mut self, rule: FlowRule) -> Result<(), CrudError> {
        validate_rule(&rule)?;
        if self.rules.len() >= FLOW_RULE_SET_MAX {
            return Err(CrudError::SetFull {
                max: FLOW_RULE_SET_MAX,
            });
        }
        if self.rules.iter().any(|r| r.name == rule.name) {
            return Err(CrudError::NameExists(rule.name));
        }
        let used = self.used_keys();
        if used + rule.max_keys as u64 > self.max_keys_budget as u64 {
            return Err(CrudError::BudgetExceeded {
                used,
                requested: rule.max_keys,
                budget: self.max_keys_budget,
            });
        }
        self.rules.push(rule);
        Ok(())
    }

    pub fn rm(&mut self, name: &str) -> Result<FlowRule, CrudError> {
        let pos = self
            .rules
            .iter()
            .position(|r| r.name == name)
            .ok_or_else(|| CrudError::NotFound(name.to_string()))?;
        Ok(self.rules.remove(pos))
    }

    pub fn list(&self) -> &[FlowRule] {
        &self.rules
    }

    pub fn show(&self, name: &str) -> Result<&FlowRule, CrudError> {
        self.rules
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| CrudError::NotFound(name.to_string()))
    }

    fn used_keys(&self) -> u64 {
        self.rules.iter().map(|r| r.max_keys as u64).sum()
    }
}
