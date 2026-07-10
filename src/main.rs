#[tokio::main]
async fn main() {
    std::process::exit(netsuite_cli::cli::cli_main().await);
}
