use serde::Serialize;

use crate::OutputFormat;

/// Print a value as table or JSON based on the output format.
pub fn print_output<T: Serialize + TableDisplay>(
    value: &T,
    format: &OutputFormat,
    compact: bool,
) {
    match format {
        OutputFormat::Table => value.print_table(),
        OutputFormat::Json => {
            if compact {
                println!("{}", serde_json::to_string(value).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(value).unwrap());
            }
        }
    }
}

/// Trait for types that can render themselves as a human-readable table.
pub trait TableDisplay {
    fn print_table(&self);
}
