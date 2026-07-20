//! The prebuilt integration record's client ID, baked into the binary at build time.
//!
//! An OAuth 2.0 client ID for a PKCE public client is an identifier, not a secret, so it is
//! safe to compile into a distributed binary. When a build carries one, `account add` works
//! without `--client-id` and nobody registering an account has to create their own NetSuite
//! integration record — they authenticate against the org's prebuilt integration instead.
//!
//! Set `NETSUITE_CLI_BUILTIN_CLIENT_ID` in the build environment (see
//! `.github/workflows/release.yml`) to embed it. Builds without it still work; every
//! `account add` just requires an explicit `--client-id` as before.

const COMPILED_CLIENT_ID: Option<&str> = option_env!("NETSUITE_CLI_BUILTIN_CLIENT_ID");

/// The client ID embedded at build time, if any. Treats an empty/whitespace value the same
/// as an unset one so `NETSUITE_CLI_BUILTIN_CLIENT_ID=""` can't embed an unusable ID.
pub fn builtin_client_id() -> Option<&'static str> {
    COMPILED_CLIENT_ID
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Resolution order for the client ID every `account add` (and cert command) needs:
/// explicit `--client-id` flag → the build's embedded client ID. Pure so both the flag-wins
/// rule and the no-builtin error stay testable regardless of the test build's environment.
pub fn resolve_client_id(
    flag: Option<&str>,
    builtin: Option<&str>,
) -> Result<String, crate::error::CliError> {
    flag.or(builtin).map(str::to_string).ok_or_else(|| {
        crate::error::CliError::Usage(
            "no client ID: pass --client-id <integration Client ID>, or use a netsuite-cli \
                 build with a built-in client ID (compiled in via NETSUITE_CLI_BUILTIN_CLIENT_ID)"
                .into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CliError;

    #[test]
    fn explicit_flag_wins_over_builtin() {
        assert_eq!(
            resolve_client_id(Some("FLAG"), Some("BUILTIN")).unwrap(),
            "FLAG"
        );
    }

    #[test]
    fn builtin_fills_in_when_flag_is_absent() {
        assert_eq!(resolve_client_id(None, Some("BUILTIN")).unwrap(), "BUILTIN");
    }

    #[test]
    fn missing_both_is_a_usage_error_naming_the_flag() {
        let error = resolve_client_id(None, None).unwrap_err();
        match error {
            CliError::Usage(message) => assert!(message.contains("--client-id")),
            other => panic!("expected Usage error, got {other:?}"),
        }
    }
}
