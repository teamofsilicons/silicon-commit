//! HTTP authentication extraction.
//!
//! Every product route accepts exactly one `Authorization` header:
//! `Bearer <Silicon Accounts access token issued to Commit>` or
//! `Proof <sap_… User verification proof>` from an app allowed to act for the
//! account (`COMMIT_PROOF_ISSUERS`). Headers of the IAM era are refused with a
//! precise message instead of being silently ignored.

use axum::http::HeaderMap;
use secrecy::SecretString;

use crate::{
    application::ports::{AuthenticationRequest, InboundCredential},
    error::AppError,
};

/// Commit's action ids, which are also the proof scopes routes require.
pub use crate::application::scopes as action;

/// Longest credential accepted (access tokens and proofs are far shorter).
const MAX_CREDENTIAL_BYTES: usize = 8_192;

/// Headers of the IAM era and of Honeycomb testing environments.
const RETIRED_HEADERS: [(&str, &str); 6] = [
    ("x-org-id", "Commit has no organizations any more"),
    (
        "x-app-id",
        "apps act for an account with Authorization: Proof <sap_…>",
    ),
    (
        "x-iam-obo-access-token",
        "apps act for an account with Authorization: Proof <sap_…>",
    ),
    (
        "x-iam-obo-access-proof",
        "apps act for an account with Authorization: Proof <sap_…>",
    ),
    (
        "x-testing-environment-key",
        "Commit has no testing environments any more",
    ),
    (
        "x-testing-app-secret",
        "Commit has no testing environments any more",
    ),
];

/// Parses the request's credential for an action.
///
/// # Errors
///
/// Returns a precise error for a missing, duplicated, malformed or retired credential.
pub fn request(
    headers: &HeaderMap,
    scope: &str,
    sensitive: bool,
) -> Result<AuthenticationRequest, AppError> {
    for (name, why) in RETIRED_HEADERS {
        if headers.contains_key(name) {
            return Err(AppError::Invalid {
                code: "retired_header".into(),
                message: format!(
                    "The {name} header is no longer accepted: {why}. Remove it and send Authorization: Bearer <Silicon Accounts access token for Commit>."
                ),
            });
        }
    }
    let credential = credential(headers)?;
    Ok(AuthenticationRequest {
        credential,
        scope: scope.to_owned(),
        sensitive,
    })
}

fn credential(headers: &HeaderMap) -> Result<InboundCredential, AppError> {
    let mut values = headers.get_all(http::header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Err(AppError::Unauthenticated);
    };
    if values.next().is_some() {
        return Err(AppError::Invalid {
            code: "duplicate_authorization".into(),
            message: "Send exactly one Authorization header.".to_owned(),
        });
    }
    let value = value.to_str().map_err(|_| malformed())?;
    let (scheme, token) = value.split_once(' ').ok_or_else(malformed)?;
    if token.is_empty()
        || token.len() > MAX_CREDENTIAL_BYTES
        || token
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(malformed());
    }
    if scheme.eq_ignore_ascii_case("bearer") {
        return Ok(InboundCredential::Bearer(SecretString::from(
            token.to_owned(),
        )));
    }
    if scheme.eq_ignore_ascii_case("proof") {
        if !token.starts_with("sap_") {
            return Err(AppError::Authentication {
                code: "proof_malformed".into(),
                message: "A proof is the sap_… token Silicon Accounts issued to the acting app (the sapr_… refresh token stays with that app).".to_owned(),
            });
        }
        return Ok(InboundCredential::Proof(SecretString::from(
            token.to_owned(),
        )));
    }
    Err(AppError::Authentication {
        code: "unsupported_authorization_scheme".into(),
        message: format!(
            "Authorization scheme `{scheme}` is not accepted. Use Bearer <Silicon Accounts access token for Commit> or Proof <sap_… proof>."
        ),
    })
}

fn malformed() -> AppError {
    AppError::Authentication {
        code: "authorization_malformed".into(),
        message: "The Authorization header must be `Bearer <token>` or `Proof <sap_…>`, with one space and no other whitespace.".to_owned(),
    }
}

/// Reads an optional single-valued header.
pub(super) fn optional_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Option<String>, AppError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(invalid_header(name));
    }
    value
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| invalid_header(name))
        })
        .transpose()
}

fn invalid_header(name: &'static str) -> AppError {
    AppError::BadRequest {
        code: format!("invalid_{}", name.replace('-', "_")).into(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::request;
    use crate::{application::ports::InboundCredential, error::AppError};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn bearer_and_proof_credentials_are_recognised() {
        let bearer = request(
            &headers(&[("authorization", "Bearer eyJ.a.b")]),
            "commit.todos.list",
            false,
        );
        assert!(matches!(
            bearer.map(|r| r.credential),
            Ok(InboundCredential::Bearer(_))
        ));
        let proof = request(
            &headers(&[("authorization", "Proof sap_abc")]),
            "commit.todos.list",
            true,
        );
        assert!(
            proof.is_ok_and(|r| r.sensitive && matches!(r.credential, InboundCredential::Proof(_)))
        );
    }

    #[test]
    fn missing_malformed_and_retired_credentials_are_refused_precisely() {
        assert!(matches!(
            request(&HeaderMap::new(), "commit.todos.list", false),
            Err(AppError::Unauthenticated)
        ));
        assert!(matches!(
            request(&headers(&[("authorization", "Basic abc")]), "s", false),
            Err(AppError::Authentication { code, .. }) if code == "unsupported_authorization_scheme"
        ));
        assert!(matches!(
            request(&headers(&[("authorization", "Proof sapr_refresh")]), "s", false),
            Err(AppError::Authentication { code, .. }) if code == "proof_malformed"
        ));
        assert!(matches!(
            request(&headers(&[("authorization", "Bearer a"), ("authorization", "Bearer b")]), "s", false),
            Err(AppError::Invalid { code, .. }) if code == "duplicate_authorization"
        ));
        assert!(matches!(
            request(&headers(&[("authorization", "Bearer a"), ("x-org-id", "tos")]), "s", false),
            Err(AppError::Invalid { code, .. }) if code == "retired_header"
        ));
        assert!(matches!(
            request(
                &headers(&[("x-iam-obo-access-token", "oba"), ("x-app-id", "interface")]),
                "s",
                false
            ),
            Err(AppError::Invalid { .. })
        ));
    }
}
