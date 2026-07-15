use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::error::CliError;

// A stalled or malicious local connection that opens the TLS session but never finishes
// sending its request must not be able to hang `account add` forever.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(30);

// Repeated TLS probe connections (e.g. from port scanners or browser cert-warning retries)
// each reset the per-connection timeout, so the accept loop also needs an overall deadline
// on the whole login attempt.
const LOGIN_FLOW_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) async fn listen_for_redirect<CallbackValue>(
    port: u16,
    parse_query: impl Fn(&str) -> Result<CallbackValue, CliError>,
) -> Result<CallbackValue, CliError> {
    let acceptor = build_loopback_tls_acceptor()?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|bind_error| {
            CliError::Network(format!(
                "cannot bind https://localhost:{port}: {bind_error}"
            ))
        })?;
    eprintln!("Waiting for the OAuth redirect on https://localhost:{port}/callback …");

    match tokio::time::timeout(
        LOGIN_FLOW_TIMEOUT,
        accept_callback(listener, acceptor, parse_query),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(CliError::Auth(
            "login timed out after 5 minutes; re-run account add (or use --paste)".into(),
        )),
    }
}

pub(crate) fn read_pasted_redirect<CallbackValue>(
    parse_query: impl Fn(&str) -> Result<CallbackValue, CliError>,
) -> Result<CallbackValue, CliError> {
    eprintln!("Paste the full redirect URL after logging in:");
    let mut pasted_line = String::new();
    std::io::stdin()
        .read_line(&mut pasted_line)
        .map_err(|read_error| CliError::Auth(format!("failed to read pasted URL: {read_error}")))?;
    let pasted_line = pasted_line.trim();
    let query = pasted_line
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or(pasted_line);
    parse_query(query)
}

async fn accept_callback<CallbackValue>(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    parse_query: impl Fn(&str) -> Result<CallbackValue, CliError>,
) -> Result<CallbackValue, CliError> {
    loop {
        let (tcp_stream, _peer_addr) = listener
            .accept()
            .await
            .map_err(|accept_error| CliError::Network(format!("accept failed: {accept_error}")))?;
        // A failed handshake is typically the browser's cert-warning probe connection —
        // ignore it and keep accepting rather than aborting the whole login attempt.
        let Ok(mut tls_stream) = acceptor.accept(tcp_stream).await else {
            continue;
        };

        let request_head =
            match tokio::time::timeout(CONNECTION_READ_TIMEOUT, read_request_head(&mut tls_stream))
                .await
            {
                Ok(Ok(head)) => head,
                Ok(Err(_)) | Err(_) => continue,
            };
        let Some(request_path) = request_line_path(&request_head) else {
            continue;
        };

        if !request_path.starts_with("/callback") {
            let _ = tls_stream.write_all(NOT_FOUND_RESPONSE).await;
            continue;
        }

        let query = request_path
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or("");
        let callback_result = parse_query(query);

        let _ = tls_stream
            .write_all(callback_response(&callback_result).as_bytes())
            .await;
        let _ = tls_stream.shutdown().await;

        return callback_result;
    }
}

fn build_loopback_tls_acceptor() -> Result<TlsAcceptor, CliError> {
    let certified_key =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).map_err(|cert_error| {
            CliError::Auth(format!("cannot generate loopback TLS cert: {cert_error}"))
        })?;
    let cert_der: CertificateDer<'static> = certified_key.cert.der().clone();
    let key_der: PrivateKeyDer<'static> = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified_key.key_pair.serialize_der(),
    ));

    let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(crypto_provider)
        .with_safe_default_protocol_versions()
        .map_err(|config_error| {
            CliError::Auth(format!(
                "cannot select TLS protocol versions: {config_error}"
            ))
        })?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|config_error| {
            CliError::Auth(format!("cannot build loopback TLS config: {config_error}"))
        })?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

const MAX_REQUEST_HEAD_BYTES: usize = 8192;

async fn read_request_head<Stream: AsyncReadExt + Unpin>(
    stream: &mut Stream,
) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "connection closed before request head",
            ));
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n")
            || buffer.len() >= MAX_REQUEST_HEAD_BYTES
        {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn request_line_path(request_head: &str) -> Option<&str> {
    request_head.lines().next()?.split_whitespace().nth(1)
}

const NOT_FOUND_RESPONSE: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Pure helper so the browser's callback page reflects the actual outcome instead of always
/// claiming success. Never interpolates raw query values into the body — only these two fixed,
/// safe strings are ever shown — so a malicious `?error=` or `?code=` value can't be reflected
/// into the response.
fn callback_response_body<CallbackValue>(
    callback_result: &Result<CallbackValue, CliError>,
) -> &'static str {
    match callback_result {
        Ok(_) => "<html><body>Login complete \u{2014} return to your terminal.</body></html>",
        Err(_) => "<html><body>Login failed \u{2014} return to your terminal.</body></html>",
    }
}

fn callback_response<CallbackValue>(callback_result: &Result<CallbackValue, CliError>) -> String {
    let body = callback_response_body(callback_result);
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_response_body_reflects_success_or_failure() {
        let success = callback_response_body(&Ok::<_, CliError>("abc123".to_string()));
        assert!(success.contains("Login complete"));

        let denied = callback_response_body(&Err::<String, _>(CliError::Auth(
            "authorization denied: access_denied".into(),
        )));
        assert!(denied.contains("Login failed"));
        assert!(!denied.contains("access_denied"));

        let state_mismatch = callback_response_body(&Err::<String, _>(CliError::Auth(
            "state mismatch in OAuth callback — possible CSRF, aborting".into(),
        )));
        assert!(state_mismatch.contains("Login failed"));
    }
}
