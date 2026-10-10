//! Silicon Accounts sign-in for Commit's own tools.
//!
//! Commit's CLI (or any tool of Commit's that runs on someone's machine) is a *public
//! client* at Silicon Accounts: it holds no app secret, only Commit's app id. A Carbon
//! signs in with the device flow (approve a code on the account site); a Silicon hands
//! over a short-lived token (`slt_…`) it minted with `silicon-accounts login --app commit -q`.
//! Both return Commit's tokens: an access token (30 minutes, `aud = commit`) and a refresh
//! token that rotates on every use. Presenting a used refresh token ends the whole sign-in,
//! so refresh one at a time and store the new pair before using it.
//!
//! This module is stateless like the rest of the crate: callers decide where tokens live.

use std::fmt;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, HeaderMap};
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountsClient, DevicePoll, OAuthError, TokenResponse};

pub use silicon_accounts_client::{AccountKind, DeviceAuthorization};

use crate::error::{Error, root_cause};

/// Commit's app id at Silicon Accounts.
pub const APP_ID: &str = "commit";
/// The production Silicon Accounts URL.
pub const DEFAULT_ACCOUNTS_URL: &str = silicon_accounts_client::DEFAULT_BASE_URL;
/// How many seconds RFC 8628 adds to the polling interval after `slow_down`.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// Largest answer read from Silicon Accounts by the direct calls.
const MAX_BODY: usize = 1024 * 1024;

/// Signs Carbons and Silicons in to Commit at Silicon Accounts, without an app secret.
#[derive(Clone)]
pub struct AccountsAuth {
    accounts: AccountsClient,
    http: reqwest::Client,
    app_id: String,
    telemetry: bool,
}

impl fmt::Debug for AccountsAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountsAuth")
            .field("accounts_url", &self.accounts.base_url().as_str())
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

/// Builder for [`AccountsAuth`].
#[derive(Clone, Debug)]
pub struct AccountsAuthBuilder {
    url: String,
    app_id: String,
    telemetry: bool,
    product: Option<String>,
}

impl AccountsAuthBuilder {
    /// The app id to sign in to (default `commit`; a development deployment may use another).
    #[must_use]
    pub fn app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self
    }

    /// When false, requests carry `X-Accounts-Telemetry: off`. Default true.
    #[must_use]
    pub fn telemetry(mut self, enabled: bool) -> Self {
        self.telemetry = enabled;
        self
    }

    /// A product token prepended to the User-Agent, e.g. `silicon-commit-cli/0.5.0`.
    #[must_use]
    pub fn user_agent(mut self, product: impl Into<String>) -> Self {
        self.product = Some(product.into());
        self
    }

    /// Builds the client. Plain `http://` is accepted only for this machine
    /// (`localhost`, `127.0.0.1`, `::1`), so tokens never cross a network unencrypted.
    pub fn build(self) -> Result<AccountsAuth, Error> {
        let app_id = self.app_id.trim().to_owned();
        if app_id.is_empty()
            || app_id.len() > 64
            || !app_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::Invalid(format!(
                "`{app_id}` is not an app id: use lowercase letters, digits, `-` and `_` (Commit's is `{APP_ID}`)."
            )));
        }
        let product = self
            .product
            .unwrap_or_else(|| format!("silicon-commit-client/{}", env!("CARGO_PKG_VERSION")));
        let accounts = AccountsClient::builder()
            .base_url(self.url)
            .telemetry(self.telemetry)
            .user_agent(product.clone())
            .build()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(product)
            .build()
            .map_err(|e| {
                Error::Invalid(format!(
                    "Could not create the HTTP client: {}.",
                    root_cause(&e)
                ))
            })?;
        Ok(AccountsAuth {
            accounts,
            http,
            app_id,
            telemetry: self.telemetry,
        })
    }
}

/// A finished sign-in: Commit's tokens and the account they belong to.
#[derive(Clone)]
#[non_exhaustive]
pub struct SignIn {
    /// Access token for the Commit API (`Authorization: Bearer …`).
    pub access_token: SecretString,
    /// Rotating refresh token (`sar_…`); store the newest one before using it.
    pub refresh_token: Option<SecretString>,
    /// Seconds until the access token expires, counted from when the answer arrived.
    pub expires_in: u64,
    /// When the sign-in ends however often it is refreshed (unix seconds), when known.
    pub refresh_expires_at: Option<i64>,
    /// Space-separated details the account shared with Commit (`profile`, `email`, …).
    pub scope: Option<String>,
    /// Who signed in.
    pub account: SignedInAccount,
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignIn")
            .field("tokens", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("scope", &self.scope)
            .field("account", &self.account)
            .finish()
    }
}

/// The account a sign-in belongs to, as Silicon Accounts shows it to Commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SignedInAccount {
    /// Permanent, case-sensitive account uuid: key everything on it.
    pub uuid: String,
    /// Current `c:`/`si:` id; it can change.
    pub id: String,
    /// Carbon or Silicon.
    pub kind: AccountKind,
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    /// The email the Carbon shared with Commit (scope `email`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// A Silicon's custodian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<AccountLink>,
}

impl SignedInAccount {
    /// Creates a value (useful in tests and for callers restoring stored sessions).
    pub fn new(uuid: impl Into<String>, id: impl Into<String>, kind: AccountKind) -> Self {
        Self {
            uuid: uuid.into(),
            id: id.into(),
            kind,
            display_name: String::new(),
            email: None,
            custodian: None,
        }
    }
}

/// A reference to another account: its permanent uuid and current id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLink {
    /// Permanent account uuid.
    pub uuid: String,
    /// Current `c:`/`si:` id.
    #[serde(default)]
    pub id: String,
}

impl SignIn {
    fn from_tokens(tokens: TokenResponse) -> Result<Self, Error> {
        let Some(account) = tokens.account else {
            return Err(Error::Invalid(
                "Silicon Accounts answered without saying which account signed in (no `account` in the token response). Retry; report it with `commit report` if it keeps happening.".into(),
            ));
        };
        let custodian = account.custodian.map(|c| AccountLink {
            uuid: c.uuid,
            id: c.id,
        });
        Ok(Self {
            access_token: SecretString::from(tokens.access_token.into_inner()),
            refresh_token: tokens
                .refresh_token
                .map(|t| SecretString::from(t.into_inner())),
            expires_in: tokens.expires_in,
            refresh_expires_at: tokens.refresh_token_expires_at.map(|t| t.unix_timestamp()),
            scope: tokens.scope,
            account: SignedInAccount {
                uuid: account.uuid,
                id: account.id,
                kind: account.kind,
                display_name: account.display_name,
                email: account.email,
                custodian,
            },
        })
    }
}

/// One poll of a device sign-in.
#[derive(Debug)]
#[non_exhaustive]
pub enum DeviceStatus {
    /// Not approved yet; poll again after the interval.
    Pending,
    /// Polling too fast: add 5 seconds to the interval.
    SlowDown,
    /// The Carbon denied the sign-in on the account site.
    Denied,
    /// The code expired (10 minutes) before anyone approved it.
    Expired,
    /// Approved.
    SignedIn(Box<SignIn>),
}

/// What [`AccountsAuth::wait_for_device_sign_in`] reports between polls.
#[derive(Debug)]
#[non_exhaustive]
pub enum DeviceEvent<'a> {
    /// Still waiting for the Carbon.
    Pending,
    /// Silicon Accounts asked to poll more slowly; the new interval.
    SlowDown {
        /// Seconds between polls from now on.
        interval: Duration,
    },
    /// A poll failed temporarily (network, 5xx, rate limit); polling continues.
    TransientError {
        /// The failure.
        error: &'a Error,
        /// Delay before the next poll.
        retry_in: Duration,
    },
}

/// The answer of a revocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// True when a live sign-in of this app was ended.
    pub revoked: bool,
    /// Why nothing was revoked (the token was unknown, already ended, or not a sign-in token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl AccountsAuth {
    /// Signs in to Commit (`commit`) at the given Silicon Accounts URL.
    pub fn new(accounts_url: impl Into<String>) -> Result<Self, Error> {
        Self::builder(accounts_url).build()
    }

    /// Starts a builder for the given Silicon Accounts URL.
    pub fn builder(accounts_url: impl Into<String>) -> AccountsAuthBuilder {
        AccountsAuthBuilder {
            url: accounts_url.into(),
            app_id: APP_ID.to_owned(),
            telemetry: true,
            product: None,
        }
    }

    /// The Silicon Accounts URL requests go to.
    pub fn accounts_url(&self) -> &url::Url {
        self.accounts.base_url()
    }

    /// The app id this client signs in to.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// `POST /v1/device/authorize` with Commit's `client_id`: starts a Carbon's sign-in.
    /// Show `user_code` and `verification_uri` (or `verification_uri_complete`), then poll
    /// with [`AccountsAuth::wait_for_device_sign_in`]. `scope` asks for extra details the
    /// Carbon may share (space-separated, e.g. `email`); `client_label` names this device in
    /// the Carbon's sign-in history.
    pub async fn start_device_sign_in(
        &self,
        scope: Option<&str>,
        client_label: Option<&str>,
    ) -> Result<DeviceAuthorization, Error> {
        Ok(self
            .accounts
            .app_device_authorize(&self.app_id, scope, client_label)
            .await?)
    }

    /// Polls a device sign-in once.
    pub async fn poll_device_sign_in(
        &self,
        device: &DeviceAuthorization,
    ) -> Result<DeviceStatus, Error> {
        let poll = self
            .accounts
            .app_device_poll(&self.app_id, device.device_code.expose())
            .await?;
        Ok(match poll {
            DevicePoll::Tokens(tokens) => {
                DeviceStatus::SignedIn(Box::new(SignIn::from_tokens(*tokens)?))
            }
            DevicePoll::SlowDown => DeviceStatus::SlowDown,
            DevicePoll::Denied => DeviceStatus::Denied,
            DevicePoll::Expired => DeviceStatus::Expired,
            _ => DeviceStatus::Pending,
        })
    }

    /// Polls a device sign-in until the Carbon approves it, honouring `interval`, adding
    /// 5 seconds after every `slow_down`, retrying temporary failures, and giving up when
    /// the code expires. A denied sign-in fails with code `access_denied`, an expired one
    /// with `expired_token`.
    pub async fn wait_for_device_sign_in(
        &self,
        device: &DeviceAuthorization,
        mut on_event: impl FnMut(DeviceEvent<'_>),
    ) -> Result<SignIn, Error> {
        let started = tokio::time::Instant::now();
        let lifetime = Duration::from_secs(device.expires_in.max(1));
        let mut interval = Duration::from_secs(device.interval.max(1));
        loop {
            tokio::time::sleep(interval).await;
            match self.poll_device_sign_in(device).await {
                Ok(DeviceStatus::SignedIn(sign_in)) => return Ok(*sign_in),
                Ok(DeviceStatus::Denied) => {
                    return Err(oauth(
                        "access_denied",
                        format!(
                            "The sign-in with code {} was denied on the account site.",
                            device.user_code
                        ),
                    ));
                }
                Ok(DeviceStatus::Expired) => return Err(expired(device)),
                Ok(DeviceStatus::SlowDown) => {
                    interval += SLOW_DOWN_STEP;
                    on_event(DeviceEvent::SlowDown { interval });
                }
                Ok(DeviceStatus::Pending) => on_event(DeviceEvent::Pending),
                Err(error) if error.is_transient() => on_event(DeviceEvent::TransientError {
                    error: &error,
                    retry_in: interval,
                }),
                Err(error) => return Err(error),
            }
            if started.elapsed() + interval > lifetime {
                return Err(expired(device));
            }
        }
    }

    /// Exchanges a Silicon's short-lived token (`slt_…`, single use, 2 minutes, minted for
    /// Commit with `silicon-accounts login --app commit -q`) for Commit's tokens, with
    /// Commit's `client_id` alone. Commit's sign-in setup has `public_client` on. The token
    /// is never logged or echoed in errors.
    pub async fn exchange_slt(&self, slt: &str) -> Result<SignIn, Error> {
        let slt = slt.trim();
        if slt.is_empty() {
            return Err(Error::Invalid(format!(
                "The short-lived token is empty. Mint one for Commit with `silicon-accounts login --app {} -q` and pass it with --slt-stdin.",
                self.app_id
            )));
        }
        if !slt.starts_with("slt_") {
            return Err(Error::Invalid(format!(
                "That is not a Silicon Accounts short-lived token: those start with `slt_`, and this looks like {}. Mint one for Commit with `silicon-accounts login --app {} -q`.",
                describe_token(slt),
                self.app_id
            )));
        }
        let form = [
            ("grant_type", silicon_accounts_client::SLT_GRANT_TYPE),
            ("slt", slt),
            ("client_id", self.app_id.as_str()),
        ];
        let tokens: TokenResponse = self.post_form("token", &form).await?;
        SignIn::from_tokens(tokens)
    }

    /// Rotates a refresh token with Commit's `client_id` alone. The old token stops working
    /// even if this answer is lost, and presenting it again ends the sign-in: store the new
    /// pair before using it, and never run two refreshes of one sign-in at once.
    pub async fn refresh(&self, refresh_token: &str) -> Result<SignIn, Error> {
        let tokens = self
            .accounts
            .refresh_app_public_client(&self.app_id, refresh_token)
            .await?;
        SignIn::from_tokens(tokens)
    }

    /// Ends the sign-in behind a refresh token (or access token) at Silicon Accounts
    /// (`POST /v1/oauth/revoke` with Commit's `client_id`). Commit hears
    /// `membership.signed_out` with reason `app_revoked`; the account's other sign-ins stay.
    /// An unknown or already-ended token answers `revoked: false` with the reason.
    pub async fn revoke(&self, token: &str) -> Result<Revocation, Error> {
        let token = token.trim();
        let hint = if token.starts_with("sar_") {
            "refresh_token"
        } else {
            "access_token"
        };
        let form = [
            ("token", token),
            ("token_type_hint", hint),
            ("client_id", self.app_id.as_str()),
        ];
        let outcome: Option<Revocation> = self.post_form("revoke", &form).await?;
        Ok(outcome.unwrap_or(Revocation {
            revoked: true,
            message: None,
        }))
    }

    /// `POST /v1/oauth/{endpoint}` with a form body, no redirects, and the service's error
    /// bodies turned into typed Silicon Accounts errors.
    async fn post_form<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        form: &[(&str, &str)],
    ) -> Result<T, Error> {
        let mut url = self.accounts.base_url().clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().extend(["v1", "oauth", endpoint]);
        }
        let what = format!("POST /v1/oauth/{endpoint}");
        let mut request = self
            .http
            .post(url)
            .header(ACCEPT, "application/json")
            .form(form);
        if !self.telemetry {
            request = request.header(silicon_accounts_client::TELEMETRY_HEADER, "off");
        }
        let response = request.send().await.map_err(|e| self.transport(e, &what))?;
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| self.transport(e, &what))?;
        if body.len() > MAX_BODY {
            return Err(Error::ResponseTooLarge);
        }
        if !status.is_success() {
            return Err(Error::Accounts(accounts_error(
                status, &headers, &body, &what,
            )));
        }
        let body: &[u8] = if body.iter().all(u8::is_ascii_whitespace) {
            b"null"
        } else {
            &body
        };
        serde_json::from_slice(body).map_err(|e| {
            Error::Invalid(format!(
                "Silicon Accounts answered {what} with HTTP {}, but not with the JSON this client expects ({e}). Check that ACCOUNTS_URL points at Silicon Accounts.",
                status.as_u16()
            ))
        })
    }

    fn transport(&self, error: reqwest::Error, what: &str) -> Error {
        let base = self
            .accounts
            .base_url()
            .as_str()
            .trim_end_matches('/')
            .to_owned();
        let error = error.without_url();
        let cause = root_cause(&error);
        let (message, hint) = if error.is_timeout() {
            (
                format!("Silicon Accounts at {base} did not answer {what} in time."),
                "Retry. For a refresh, the old refresh token may already be used up; if the next try says so, sign in again.",
            )
        } else {
            (
                format!("Could not connect to Silicon Accounts at {base} ({what}): {cause}."),
                "Check ACCOUNTS_URL and your network, then retry.",
            )
        };
        Error::Accounts(silicon_accounts_client::Error::Http {
            message,
            hint: hint.to_owned(),
            source: error,
        })
    }
}

fn oauth(code: &str, description: String) -> Error {
    Error::Accounts(OAuthError::new(400, code, Some(description)).into())
}

fn expired(device: &DeviceAuthorization) -> Error {
    oauth(
        "expired_token",
        format!(
            "The sign-in code {} expired before it was approved (codes last {} minutes).",
            device.user_code,
            device.expires_in.div_ceil(60).max(1)
        ),
    )
}

/// Turns a non-2xx Silicon Accounts answer into its typed error (RFC 6749 body, the
/// standard `{"error":{…}}` body, or a foreign body that is not echoed).
fn accounts_error(
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    what: &str,
) -> silicon_accounts_client::Error {
    let request_id = headers
        .get(silicon_accounts_client::REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    if let Some(code) = error.and_then(serde_json::Value::as_str) {
        let description = parsed
            .as_ref()
            .and_then(|v| v.get("error_description"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let mut oauth = OAuthError::new(status.as_u16(), code, description);
        oauth.request_id = request_id;
        return oauth.into();
    }
    let text = |key: &str| {
        error
            .and_then(|e| e.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let code = text("code").unwrap_or_else(|| format!("http_{}", status.as_u16()));
    let message = text("message").unwrap_or_else(|| {
        format!(
            "Silicon Accounts answered {what} with HTTP {} {}.",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
    });
    let mut api = silicon_accounts_client::ApiError::new(status.as_u16(), code, message);
    api.hint = text("hint");
    api.details = error
        .and_then(|e| e.get("details"))
        .filter(|d| !d.is_null())
        .cloned();
    api.request_id = request_id;
    api.into()
}

/// What a value that is not a short-lived token looks like, without echoing it.
fn describe_token(value: &str) -> &'static str {
    match value {
        v if v.starts_with("sar_") => "a refresh token",
        v if v.starts_with("sapr_") => "a proof refresh token",
        v if v.starts_with("sap_") => "a proof",
        v if v.starts_with("stk-") || v.starts_with("stk_") => {
            "a Silicon's STK (never hand an STK to an app; mint a short-lived token instead)"
        }
        v if v.starts_with("eyJ") => "a JWT (an access or id token)",
        v if v.starts_with("oac_") || v.starts_with("oat_") || v.starts_with("ort_") => {
            "a token of the previous sign-in system, which Commit no longer accepts"
        }
        _ => "something else",
    }
}

/// Why Silicon Accounts refused a short-lived token or refresh token (`invalid_grant`),
/// from its description. `None` when the error is not such a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignInRefusal {
    /// The token was already used (short-lived tokens work once).
    AlreadyUsed,
    /// The token expired (short-lived tokens last 2 minutes, sign-ins at most 900 days).
    Expired,
    /// The token was issued for another app.
    WrongApp,
    /// The token is unknown (mistyped, or from another Silicon Accounts environment).
    Unknown,
    /// The value is not that kind of token.
    Malformed,
    /// The sign-in behind it ended (signed out, STK rotated, trust removed, reuse detected).
    SignInEnded,
    /// The account was deleted, suspended, or removed Commit's access.
    AccountInactive,
    /// Another reason; read the message.
    Other,
}

impl SignInRefusal {
    /// Classifies an error; `None` unless it is an `invalid_grant` refusal.
    pub fn of(error: &Error) -> Option<Self> {
        let Error::Accounts(inner) = error else {
            return None;
        };
        let oauth = inner.as_oauth().filter(|o| o.error == "invalid_grant")?;
        let text = oauth
            .description
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        Some(
            if text.contains("already used") || text.contains("already exchanged") {
                Self::AlreadyUsed
            } else if text.contains("issued for the app") || text.contains("different app") {
                Self::WrongApp
            } else if text.contains("must be a") {
                Self::Malformed
            } else if text.contains("not known") {
                Self::Unknown
            } else if text.contains("expired") {
                Self::Expired
            } else if text.contains("revoked")
                || text.contains("rotated")
                || text.contains("trust")
                || text.contains("ended")
            {
                Self::SignInEnded
            } else if text.contains("no longer exists")
                || text.contains("can't be refreshed")
                || text.contains("removed")
            {
                Self::AccountInactive
            } else {
                Self::Other
            },
        )
    }

    /// Stable snake_case name, e.g. `already_used`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyUsed => "already_used",
            Self::Expired => "expired",
            Self::WrongApp => "wrong_app",
            Self::Unknown => "unknown",
            Self::Malformed => "malformed",
            Self::SignInEnded => "sign_in_ended",
            Self::AccountInactive => "account_inactive",
            Self::Other => "refused",
        }
    }
}

pub use silicon_accounts_client::Claims;

/// Reads an access token's claims WITHOUT verifying it: for display only (which account,
/// which app, until when). Never authorize anything with the result; Commit verifies every
/// token itself. `None` when the value is not a JWT.
pub fn peek_claims(token: &str) -> Option<Claims> {
    use base64::Engine as _;
    let mut parts = token.trim().split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
