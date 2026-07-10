mod error;
mod output;
mod account;
mod config;

use error::CliError;

#[tokio::main]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    match execute().await {
        Ok(()) => 0,
        Err(error) => {
            output::print_error(&error);
            error.exit_code() as i32
        }
    }
}

async fn execute() -> Result<(), CliError> {
    Err(CliError::Usage("no command implemented yet".into()))
}
