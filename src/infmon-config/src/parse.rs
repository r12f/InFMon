use std::path::Path;

use thiserror::Error;

use crate::crud::{CrudError, FlowRuleSet};
use crate::model::Config;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Crud(#[from] CrudError),
}

/// Parse a YAML string into Config
pub fn parse_yaml(input: &str) -> Result<Config, ParseError> {
    Ok(serde_yaml::from_str(input)?)
}

/// Parse a YAML file into Config
pub fn parse_yaml_file(path: &Path) -> Result<Config, ParseError> {
    let content = std::fs::read_to_string(path)?;
    parse_yaml(&content)
}

/// Parse and validate in one step
pub fn load_config(path: &Path) -> Result<FlowRuleSet, ConfigError> {
    let config = parse_yaml_file(path)?;
    Ok(FlowRuleSet::from_config(&config)?)
}
