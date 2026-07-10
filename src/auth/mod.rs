pub mod authcode;
pub mod m2m;

use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

use crate::error::CliError;

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

// Object-safety note: NsClient (Task 5) needs Arc<dyn TokenProvider>. Async trait methods
// aren't object-safe without boxing, so access_token returns a boxed future explicitly
// rather than using `impl Future` (which would require `Self: Sized`).
pub trait TokenProvider: Send + Sync {
    fn access_token<'life>(
        &'life self,
    ) -> Pin<Box<dyn Future<Output = Result<String, CliError>> + Send + 'life>>;
    fn invalidate(&self);
}
