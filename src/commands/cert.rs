//! M2M certificate lifecycle: local generation of a key/certificate pair, plus NetSuite's
//! OAuth 2.0 Client Credentials certificate rotation API (list / upload / revoke).
//!
//! The rotation API lives on the restlets domain:
//!   `https://<acct>.restlets.api.netsuite.com/services/rest/auth/oauth2/v1/clients/<clientId>/certificates`
//! and accepts any access token with the REST web services scope — including one from the
//! auth-code flow. That is what makes UI-free M2M bootstrap possible: log in once with
//! auth-code, upload a certificate over that token, then register an m2m account with the
//! returned certificate id. Requires the NetSuite permission "Manage own OAuth 2.0 Client
//! Credentials certificates" (Setup type) on the logged-in role.

use std::path::Path;

use serde_json::{Value, json};

use crate::client::NsClient;
use crate::error::CliError;

/// NetSuite rejects certificates whose validity exceeds two years.
pub const MAX_VALIDITY_DAYS: u64 = 730;

pub fn generate(
    key_out: &Path,
    cert_out: &Path,
    days: u64,
    common_name: &str,
    force: bool,
) -> Result<Value, CliError> {
    if days == 0 || days > MAX_VALIDITY_DAYS {
        return Err(CliError::Usage(format!(
            "--days must be between 1 and {MAX_VALIDITY_DAYS} (NetSuite's two-year certificate \
             validity ceiling), got {days}"
        )));
    }
    if !force {
        for existing in [key_out, cert_out] {
            if existing.exists() {
                return Err(CliError::Usage(format!(
                    "{} already exists; pass --force to overwrite it (overwriting a key whose \
                     certificate is still registered breaks that M2M setup)",
                    existing.display()
                )));
            }
        }
    }

    // EC P-256 on purpose: NetSuite accepts it, assertions sign fast, and the PEM stays small
    // enough for Windows Credential Manager's blob limit (RSA-4096 does not — see add_m2m).
    let key_pair = rcgen::KeyPair::generate().map_err(|generate_error| {
        CliError::Usage(format!("cannot generate EC P-256 key: {generate_error}"))
    })?;
    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name);
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before + time::Duration::days(days as i64);
    let valid_until = params
        .not_after
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339-formattable timestamp");
    let certificate = params.self_signed(&key_pair).map_err(|sign_error| {
        CliError::Usage(format!("cannot self-sign certificate: {sign_error}"))
    })?;

    write_private(key_out, &key_pair.serialize_pem())?;
    write_file(cert_out, &certificate.pem())?;
    Ok(json!({
        "keyPath": key_out.display().to_string(),
        "certPath": cert_out.display().to_string(),
        "algorithm": "EC P-256",
        "validDays": days,
        "validUntil": valid_until,
    }))
}

pub async fn list(
    client: &NsClient,
    restlet_base: &str,
    client_id: &str,
) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            &certificates_url(restlet_base, client_id),
            &[],
            &[("Accept", "application/json")],
            None,
        )
        .await?;
    let certificates = response.body.unwrap_or_else(|| json!([]));
    Ok(json!({"certificates": certificates}))
}

pub async fn upload(
    client: &NsClient,
    restlet_base: &str,
    client_id: &str,
    cert_path: &Path,
    entity: &str,
    role: &str,
) -> Result<Value, CliError> {
    let pem = std::fs::read_to_string(cert_path).map_err(|read_error| {
        CliError::Usage(format!(
            "cannot read certificate {}: {read_error}",
            cert_path.display()
        ))
    })?;
    if pem.contains("PRIVATE KEY") {
        return Err(CliError::Usage(format!(
            "{} contains a PRIVATE KEY — never upload the key; pass the certificate PEM \
             (the *-cert.pem file from `account cert generate`)",
            cert_path.display()
        )));
    }
    if !pem.contains("BEGIN CERTIFICATE") {
        return Err(CliError::Usage(format!(
            "{} is not a PEM certificate (no BEGIN CERTIFICATE block)",
            cert_path.display()
        )));
    }
    let body = json!({
        "fileContent": pem,
        "entity": id_value(entity),
        "role": id_value(role),
    });
    let response = client
        .request(
            reqwest::Method::POST,
            &certificates_url(restlet_base, client_id),
            &[],
            &[("Accept", "application/json")],
            Some(&body),
        )
        .await?;
    let details = response.body.unwrap_or(Value::Null);
    let certificate_id = details
        .get("certificate_id")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({"certificateId": certificate_id, "details": details}))
}

pub async fn revoke(
    client: &NsClient,
    restlet_base: &str,
    client_id: &str,
    certificate_id: &str,
) -> Result<Value, CliError> {
    // The revoke response is plain text ("Successfully revoked"), not JSON; NsClient maps
    // that to body: None and this returns a synthesized JSON result instead.
    client
        .request(
            reqwest::Method::POST,
            &format!(
                "{}/{certificate_id}/revoke",
                certificates_url(restlet_base, client_id)
            ),
            &[],
            &[],
            None,
        )
        .await?;
    Ok(json!({"revoked": true, "certificateId": certificate_id}))
}

fn certificates_url(restlet_base: &str, client_id: &str) -> String {
    format!(
        "{}/services/rest/auth/oauth2/v1/clients/{client_id}/certificates",
        restlet_base.trim_end_matches('/')
    )
}

/// NetSuite's documented example sends ids in mixed representations (`"role": 1000,
/// "entity": "-5"`); send numerics as numbers and fall back to strings for anything else.
fn id_value(raw: &str) -> Value {
    raw.parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_parse_error| Value::from(raw))
}

fn write_file(path: &Path, contents: &str) -> Result<(), CliError> {
    std::fs::write(path, contents).map_err(|write_error| {
        CliError::Usage(format!("cannot write {}: {write_error}", path.display()))
    })
}

/// Same as `write_file` but owner-only (0600) on Unix — this is the private key.
fn write_private(path: &Path, contents: &str) -> Result<(), CliError> {
    write_file(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |permission_error| {
                CliError::Usage(format!(
                    "cannot restrict permissions on {}: {permission_error}",
                    path.display()
                ))
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificates_url_targets_the_rotation_endpoint() {
        assert_eq!(
            certificates_url("https://123456-sb1.restlets.api.netsuite.com", "CID123"),
            "https://123456-sb1.restlets.api.netsuite.com/services/rest/auth/oauth2/v1/clients/CID123/certificates"
        );
    }

    #[test]
    fn id_value_sends_numerics_as_numbers_and_the_rest_as_strings() {
        assert_eq!(id_value("1000"), json!(1000));
        assert_eq!(id_value("-5"), json!(-5));
        assert_eq!(id_value("abc"), json!("abc"));
    }

    #[test]
    fn generate_writes_a_usable_key_and_certificate() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_out = temp_dir.path().join("key.pem");
        let cert_out = temp_dir.path().join("cert.pem");

        let result = generate(&key_out, &cert_out, 730, "netsuite-cli", false).unwrap();
        assert_eq!(result["algorithm"], "EC P-256");
        assert_eq!(result["validDays"], 730);

        let cert_pem = std::fs::read_to_string(&cert_out).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        let key_pem = std::fs::read_to_string(&key_out).unwrap();
        assert!(key_pem.contains("PRIVATE KEY"));
        // The key must be exactly what the M2M token flow can sign with.
        let throwaway = crate::auth::m2m::M2mConfig {
            token_url: "https://validate.invalid/token".into(),
            client_id: "CID".into(),
            cert_id: "KID".into(),
            private_key_pem: key_pem,
            scopes: vec!["rest_webservices".into()],
        };
        crate::auth::m2m::build_assertion(&throwaway, 0).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_out).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "private key must be owner-only");
        }
    }

    #[test]
    fn generate_refuses_to_overwrite_without_force() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_out = temp_dir.path().join("key.pem");
        let cert_out = temp_dir.path().join("cert.pem");
        std::fs::write(&key_out, "existing").unwrap();

        let error = generate(&key_out, &cert_out, 730, "cn", false).unwrap_err();
        match error {
            CliError::Usage(message) => assert!(message.contains("--force")),
            other => panic!("expected Usage error, got {other:?}"),
        }

        generate(&key_out, &cert_out, 730, "cn", true).unwrap();
        assert!(
            std::fs::read_to_string(&key_out)
                .unwrap()
                .contains("PRIVATE KEY")
        );
    }

    #[test]
    fn generate_rejects_validity_beyond_netsuite_ceiling() {
        let temp_dir = tempfile::tempdir().unwrap();
        let error = generate(
            &temp_dir.path().join("key.pem"),
            &temp_dir.path().join("cert.pem"),
            MAX_VALIDITY_DAYS + 1,
            "cn",
            false,
        )
        .unwrap_err();
        assert!(matches!(error, CliError::Usage(_)));
        let zero_days = generate(
            &temp_dir.path().join("key.pem"),
            &temp_dir.path().join("cert.pem"),
            0,
            "cn",
            false,
        );
        assert!(zero_days.is_err());
    }
}
