use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use netsuite_cli::auth::TokenProvider;
use netsuite_cli::client::NsClient;
use netsuite_cli::error::CliError;
use wiremock::MockServer;

pub struct StaticToken;

impl TokenProvider for StaticToken {
    fn access_token<'life>(
        &'life self,
    ) -> Pin<Box<dyn Future<Output = Result<String, CliError>> + Send + 'life>> {
        Box::pin(async { Ok("TEST_TOKEN".into()) })
    }
    fn invalidate(&self) {}
}

pub fn client_for(server: &MockServer) -> NsClient {
    NsClient::new(reqwest::Client::new(), server.uri(), Arc::new(StaticToken))
}
