pub mod account;
pub mod config_cmd;
pub mod describe;
pub mod job;
pub mod raw;
pub mod record;
pub mod restlet;
pub mod saved_search;
pub mod skill;
pub mod suiteql;
pub mod system;
pub mod update;

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

/// Parses repeated `--param`/`--query key=value` flags; used by the restlet and raw commands.
pub fn parse_key_value_pairs(
    pairs: &[String],
    flag_name: &str,
) -> Result<Vec<(String, String)>, CliError> {
    pairs
        .iter()
        .map(|pair| {
            let (key, value) = pair.split_once('=').ok_or_else(|| {
                CliError::Usage(format!(
                    "{flag_name} must be in key=value form, got '{pair}'"
                ))
            })?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Parses repeated `--header 'Name: value'` flags for the raw command.
pub fn parse_header_pairs(pairs: &[String]) -> Result<Vec<(String, String)>, CliError> {
    pairs
        .iter()
        .map(|pair| {
            let (name, value) = pair.split_once(':').ok_or_else(|| {
                CliError::Usage(format!(
                    "--header must be in 'Name: value' form, got '{pair}'"
                ))
            })?;
            Ok((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn reads_inline_json() {
        let parsed = read_data_arg(r#"{"companyName": "Acme"}"#).unwrap();
        assert_eq!(parsed, serde_json::json!({"companyName": "Acme"}));
    }

    #[test]
    fn reads_json_from_file_argument() {
        let mut temp_file = tempfile::NamedTempFile::new().unwrap();
        write!(temp_file, r#"{{"companyName": "Acme"}}"#).unwrap();
        let file_arg = format!("@{}", temp_file.path().display());

        let parsed = read_data_arg(&file_arg).unwrap();
        assert_eq!(parsed, serde_json::json!({"companyName": "Acme"}));
    }

    #[test]
    fn rejects_invalid_json() {
        let parse_result = read_data_arg("not json");
        assert!(matches!(parse_result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_missing_file() {
        let parse_result = read_data_arg("@/nonexistent");
        assert!(matches!(parse_result, Err(CliError::Usage(_))));
    }
}
