pub mod record;

use std::io::Read;

use serde_json::Value;

use crate::error::CliError;

pub fn read_data_arg(raw: &str) -> Result<Value, CliError> {
    let text = if raw == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|io_error| CliError::Usage(format!("cannot read stdin: {io_error}")))?;
        buffer
    } else if let Some(file_path) = raw.strip_prefix('@') {
        std::fs::read_to_string(file_path)
            .map_err(|io_error| CliError::Usage(format!("cannot read {file_path}: {io_error}")))?
    } else {
        raw.to_string()
    };
    serde_json::from_str(&text)
        .map_err(|parse_error| CliError::Usage(format!("--data is not valid JSON: {parse_error}")))
}
