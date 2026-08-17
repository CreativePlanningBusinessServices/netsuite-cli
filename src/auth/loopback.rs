use std::io::ErrorKind;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::CliError;

// A stalled or malicious local connection that opens the socket but never finishes sending
// its request must not be able to hang `account add` forever.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(30);

// Repeated stray connections (e.g. from port scanners or a browser prefetch) each reset the
// per-connection timeout, so the accept loop also needs an overall deadline on the whole
// login attempt.
const LOGIN_FLOW_TIMEOUT: Duration = Duration::from_secs(300);

/// One-shot loopback listener for the OAuth redirect, plain HTTP on `127.0.0.1` as RFC 8252
/// §7.3 prescribes for native apps: the redirect never leaves the machine, and the response
/// carries no secret of its own — the authorization code it does carry is bound to this
/// process by PKCE and to this attempt by `state`. Serving TLS here instead would mean a
/// throwaway self-signed cert and an interstitial browser warning mid-login.
pub(crate) async fn listen_for_redirect<CallbackValue>(
    port: u16,
    parse_query: impl Fn(&str) -> Result<CallbackValue, CliError>,
) -> Result<CallbackValue, CliError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|bind_error| {
            CliError::Network(format!("cannot bind http://localhost:{port}: {bind_error}"))
        })?;
    eprintln!("Waiting for the OAuth redirect on http://localhost:{port}/callback …");

    match tokio::time::timeout(LOGIN_FLOW_TIMEOUT, accept_callback(listener, parse_query)).await {
        Ok(result) => result,
        Err(_) => Err(CliError::Auth(
            "login timed out after 5 minutes; re-run the command (or use --paste)".into(),
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
    parse_query: impl Fn(&str) -> Result<CallbackValue, CliError>,
) -> Result<CallbackValue, CliError> {
    loop {
        let (mut tcp_stream, _peer_addr) = listener
            .accept()
            .await
            .map_err(|accept_error| CliError::Network(format!("accept failed: {accept_error}")))?;

        let request_head =
            match tokio::time::timeout(CONNECTION_READ_TIMEOUT, read_request_head(&mut tcp_stream))
                .await
            {
                Ok(Ok(head)) => head,
                Ok(Err(_)) | Err(_) => continue,
            };
        let Some(request_path) = request_line_path(&request_head) else {
            continue;
        };

        if !request_path.starts_with("/callback") {
            let _ = tcp_stream.write_all(NOT_FOUND_RESPONSE).await;
            continue;
        }

        let query = request_path
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or("");
        let callback_result = parse_query(query);

        let _ = tcp_stream
            .write_all(callback_response(&callback_result).as_bytes())
            .await;
        let _ = tcp_stream.shutdown().await;

        return callback_result;
    }
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
        Ok(_) => "<html><body>Login complete. Return to your terminal.</body></html>",
        Err(_) => "<html><body>Login failed. Return to your terminal.</body></html>",
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

    /// Ask the OS for a free port, then hand it back — the listener under test takes a port
    /// number rather than choosing one, and a hardcoded port would collide with whatever else
    /// the machine is running.
    async fn free_loopback_port() -> u16 {
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        probe.local_addr().unwrap().port()
    }

    /// The whole point of serving plain HTTP: an ordinary browser request now completes the
    /// login. Under TLS this path could only be exercised by the live smoke test.
    #[tokio::test]
    async fn listener_answers_the_callback_request_and_returns_the_parsed_query() {
        let port = free_loopback_port().await;
        let listening = tokio::spawn(listen_for_redirect(port, |query: &str| {
            Ok(query.to_string())
        }));

        // The listener binds inside the spawned task, so retry the request until it's up.
        let mut response = None;
        for _ in 0..50 {
            match reqwest::get(format!("http://127.0.0.1:{port}/callback?code=abc123")).await {
                Ok(ok) => {
                    response = Some(ok);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let response = response.expect("listener never accepted a connection");
        assert!(response.status().is_success());
        assert!(response.text().await.unwrap().contains("Login complete"));
        assert_eq!(listening.await.unwrap().unwrap(), "code=abc123");
    }

    /// A stray request (favicon fetch, port scan, browser prefetch) must 404 without ending the
    /// login attempt — the real redirect can still arrive afterward on the same listener.
    #[tokio::test]
    async fn listener_404s_other_paths_and_keeps_waiting_for_the_callback() {
        let port = free_loopback_port().await;
        let listening = tokio::spawn(listen_for_redirect(port, |query: &str| {
            Ok(query.to_string())
        }));

        let mut stray = None;
        for _ in 0..50 {
            match reqwest::get(format!("http://127.0.0.1:{port}/favicon.ico")).await {
                Ok(ok) => {
                    stray = Some(ok);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert_eq!(
            stray.expect("no response to the stray request").status(),
            404
        );

        let callback = reqwest::get(format!("http://127.0.0.1:{port}/callback?code=abc123"))
            .await
            .unwrap();
        assert!(callback.status().is_success());
        assert_eq!(listening.await.unwrap().unwrap(), "code=abc123");
    }
}
