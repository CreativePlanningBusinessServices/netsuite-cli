use base64::Engine;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

pub fn token_passport_signature(
    account_id: &str,
    consumer_key: &str,
    token_id: &str,
    nonce: &str,
    timestamp: u64,
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    let base_string = format!("{account_id}&{consumer_key}&{token_id}&{nonce}&{timestamp}");
    let signing_key = format!("{consumer_secret}&{token_secret}");
    hmac_sha256_base64(&signing_key, &base_string)
}

pub fn oauth_signature(
    http_method: &str,
    url: &str,
    params: &[(&str, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    let mut sorted_params: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    sorted_params.sort();
    let parameter_string = sorted_params
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let base_string = format!(
        "{http_method}&{}&{}",
        percent_encode(url),
        percent_encode(&parameter_string)
    );
    let signing_key = format!(
        "{}&{}",
        percent_encode(consumer_secret),
        percent_encode(token_secret)
    );
    hmac_sha256_base64(&signing_key, &base_string)
}

pub fn oauth_authorization_header(params: &[(&str, String)], signature: &str) -> String {
    let mut header_params: Vec<String> = params
        .iter()
        .map(|(name, value)| format!(r#"{}="{}""#, name, percent_encode(value)))
        .collect();
    header_params.push(format!(
        r#"oauth_signature="{}""#,
        percent_encode(signature)
    ));
    format!("OAuth {}", header_params.join(", "))
}

pub fn hmac_sha256_base64(key: &str, message: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

pub fn percent_encode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub fn generate_nonce() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..20)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

pub fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after 1970")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_passport_signature_matches_reference_vector() {
        // base string: account&consumerKey&token&nonce&timestamp
        // key:         consumerSecret&tokenSecret
        let signature = token_passport_signature(
            "1234567_SB1",
            "consumerkey123",
            "tokenid456",
            "ABCDEFGHIJKLMNOPQRST",
            1_700_000_000,
            "consumersecret789",
            "tokensecret012",
        );
        assert_eq!(signature, "8VL7CiUwRAgjw1e1P/e6GkZsWf1Bmc6T3J+DXl7WLc0=");
    }

    #[test]
    fn oauth_signature_matches_reference_vector_for_request_token() {
        let params = [
            (
                "oauth_callback",
                "https://localhost:8899/callback".to_string(),
            ),
            ("oauth_consumer_key", "consumerkey123".to_string()),
            ("oauth_nonce", "ABCDEFGHIJKLMNOPQRST".to_string()),
            ("oauth_signature_method", "HMAC-SHA256".to_string()),
            ("oauth_timestamp", "1700000000".to_string()),
            ("oauth_version", "1.0".to_string()),
        ];
        let signature = oauth_signature(
            "POST",
            "https://1234567-sb1.restlets.api.netsuite.com/rest/requesttoken",
            &params,
            "consumersecret789",
            "",
        );
        assert_eq!(signature, "5sgbCQr3mRLrl1Jgmkg/UnNLwzGofJ5jkktNvRVQlYE=");
    }

    #[test]
    fn oauth_signature_matches_reference_vector_for_access_token() {
        let params = [
            ("oauth_consumer_key", "consumerkey123".to_string()),
            ("oauth_nonce", "ABCDEFGHIJKLMNOPQRST".to_string()),
            ("oauth_signature_method", "HMAC-SHA256".to_string()),
            ("oauth_timestamp", "1700000000".to_string()),
            ("oauth_token", "reqtoken111".to_string()),
            ("oauth_verifier", "verifier222".to_string()),
            ("oauth_version", "1.0".to_string()),
        ];
        let signature = oauth_signature(
            "POST",
            "https://1234567-sb1.restlets.api.netsuite.com/rest/accesstoken",
            &params,
            "consumersecret789",
            "reqtokensecret333",
        );
        assert_eq!(signature, "lH8IsRMocNRdk2TflE43sLDNU0MYzbberQZJgokyLKI=");
    }

    #[test]
    fn percent_encoding_covers_rfc3986_reserved_characters() {
        assert_eq!(
            percent_encode("https://x/y?a=b&c"),
            "https%3A%2F%2Fx%2Fy%3Fa%3Db%26c"
        );
        assert_eq!(percent_encode("safe-._~AZaz09"), "safe-._~AZaz09");
        assert_eq!(percent_encode("sp ace+plus"), "sp%20ace%2Bplus");
    }

    #[test]
    fn nonce_is_twenty_alphanumeric_characters() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), 20);
        assert!(nonce.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_ne!(generate_nonce(), nonce);
    }

    #[test]
    fn authorization_header_percent_encodes_values_and_appends_signature() {
        let header = oauth_authorization_header(
            &[("oauth_consumer_key", "key/with/slash".to_string())],
            "sig+base64=",
        );
        assert!(header.starts_with("OAuth "));
        assert!(header.contains(r#"oauth_consumer_key="key%2Fwith%2Fslash""#));
        assert!(header.ends_with(r#"oauth_signature="sig%2Bbase64%3D""#));
    }
}
