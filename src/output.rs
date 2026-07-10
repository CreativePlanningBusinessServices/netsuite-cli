use crate::error::CliError;
use serde_json::Value;

pub fn print_json(value: &Value, pretty: bool) {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value).expect("serializable"));
    } else {
        println!("{}", serde_json::to_string(value).expect("serializable"));
    }
}

pub fn print_error(error: &CliError) {
    eprintln!("{}", error.to_json());
}
