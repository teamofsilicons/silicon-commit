//! The saved sign-in of a profile: `<profile dir>/session.json` (mode 0600, written
//! atomically) and `session.lock`, held around every change so concurrent commands rotate a
//! refresh token once. A used refresh token ends the whole sign-in at Silicon Accounts, so a
//! refresh always happens under the lock and its result is saved before it is used.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_commit_client::auth::{APP_ID, AccountsAuth, SignIn, SignInRefusal, SignedInAccount};
use silicon_commit_client::{Error as ClientError, ExposeSecret as _};

use crate::output::{CARBON_LOGIN, CliError, SILICON_LOGIN, now, rfc3339};
use crate::state;

/// The session file format written by this version.
pub const VERSION: u32 = 2;
/// Refresh when the access token has less than this many seconds left.
pub const REFRESH_MARGIN: i64 = 60;

/// One profile's sign-in.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub version: u32,
    pub app_id: String,
    pub accounts_url: String,
    pub api_url: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Access token expiry (unix seconds).
    pub expires_at: i64,
    /// When the sign-in ends however often it is refreshed (unix seconds).
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    pub account: SignedInAccount,
    /// `device` (Carbon) or `slt` (short-lived token).
    pub method: String,
    pub signed_in_at: i64,
    #[serde(default)]
    pub refreshed_at: Option<i64>,
    /// Set while a refresh is in flight; still set afterwards means its answer was lost.
    #[serde(default)]
    pub refresh_started_at: Option<i64>,
    /// Set when Silicon Accounts refused the refresh token for good.
    #[serde(default)]
    pub ended: Option<Ended>,
}

/// Why a saved sign-in stopped working.
#[derive(Clone, Serialize, Deserialize)]
pub struct Ended {
    pub at: i64,
    pub reason: String,
    pub message: String,
}

impl StoredSession {
    /// A new session from a finished sign-in, received at `received_at`.
    pub fn new(
        sign_in: &SignIn,
        accounts_url: &str,
        api_url: &str,
        method: &str,
        started_at: i64,
    ) -> Self {
        let mut session = Self {
            version: VERSION,
            app_id: APP_ID.to_owned(),
            accounts_url: accounts_url.trim_end_matches('/').to_owned(),
            api_url: api_url.trim_end_matches('/').to_owned(),
            access_token: String::new(),
            refresh_token: None,
            expires_at: 0,
            refresh_expires_at: None,
            scope: None,
            account: sign_in.account.clone(),
            method: method.to_owned(),
            signed_in_at: now(),
            refreshed_at: None,
            refresh_started_at: None,
            ended: None,
        };
        session.apply(sign_in, started_at);
        session.refreshed_at = None;
        session
    }

    /// Takes the tokens and account details of a sign-in or refresh started at `started_at`.
    fn apply(&mut self, sign_in: &SignIn, started_at: i64) {
        sign_in
            .access_token
            .expose_secret()
            .clone_into(&mut self.access_token);
        if let Some(refresh) = &sign_in.refresh_token {
            self.refresh_token = Some(refresh.expose_secret().to_owned());
        }
        self.expires_at = started_at.saturating_add(i64::try_from(sign_in.expires_in).unwrap_or(0));
        self.refresh_expires_at = sign_in.refresh_expires_at.or(self.refresh_expires_at);
        if sign_in.scope.is_some() {
            self.scope.clone_from(&sign_in.scope);
        }
        self.account = sign_in.account.clone();
        self.refreshed_at = Some(now());
        self.refresh_started_at = None;
    }

    /// True when the access token has less than a minute left.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at <= now().saturating_add(REFRESH_MARGIN)
    }

    /// `c:ada (Ada), a Carbon`.
    pub fn who(&self) -> String {
        let name = if self.account.display_name.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.account.display_name)
        };
        format!("{}{name}, a {}", self.account.id, self.account.kind.title())
    }

    /// The `login status --json` answer for this session.
    pub fn status_json(&self, verified: bool) -> Value {
        let mut value = json!({
            "authenticated": true,
            "uuid": self.account.uuid,
            "id": self.account.id,
            "kind": self.account.kind.as_str(),
            "expires_at": rfc3339(self.expires_at),
            "refresh_expires_at": self.refresh_expires_at.map(rfc3339),
            "verified": verified,
            "profile": state::profile(),
            "api_url": self.api_url,
            "accounts_url": self.accounts_url,
        });
        if !self.account.display_name.is_empty() {
            value["display_name"] = json!(self.account.display_name);
        }
        if let Some(custodian) = &self.account.custodian {
            value["custodian"] = json!({ "uuid": custodian.uuid, "id": custodian.id });
        }
        value
    }

    /// The error for a session that ended.
    pub fn ended_error(&self) -> CliError {
        let (reason, message) = self
            .ended
            .as_ref()
            .map_or(("session_ended", "it ended"), |e| {
                (e.reason.as_str(), e.message.as_str())
            });
        CliError::new(
            "session_ended",
            format!(
                "Your Commit sign-in as {} has ended: {message}",
                self.account.id
            ),
        )
        .reason(reason)
        .hint(sign_in_again())
    }
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("tokens", &"<redacted>")
            .field("account", &self.account)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// What the session file holds.
pub enum Loaded {
    Missing,
    Session(Box<StoredSession>),
    /// A file from the previous sign-in system (organizations); it cannot be used.
    Legacy,
    /// Not JSON, or not a format this version knows.
    Unreadable,
}

pub fn sign_in_again() -> String {
    format!("Sign in again. Carbon: `{CARBON_LOGIN}`. Silicon: `{SILICON_LOGIN}`.")
}

impl Loaded {
    /// The session, or the precise reason there is none.
    pub fn require(self) -> Result<StoredSession, CliError> {
        match self {
            Self::Session(session) if session.ended.is_some() => Err(session.ended_error()),
            Self::Session(session) => Ok(*session),
            Self::Missing => Err(CliError::not_signed_in()),
            Self::Legacy => Err(legacy_error()),
            Self::Unreadable => Err(unreadable_error()),
        }
    }
}

pub fn legacy_error() -> CliError {
    CliError::new(
        "legacy_session",
        format!(
            "The saved session of profile `{}` is from Commit's previous sign-in system, which no longer works.",
            state::profile()
        ),
    )
    .hint(sign_in_again())
}

pub fn unreadable_error() -> CliError {
    CliError::new(
        "unreadable_session",
        format!(
            "The saved session file {} is damaged or in an unknown format, so it was ignored.",
            path().display()
        ),
    )
    .hint(format!(
        "{} (signing in replaces the file; `commit logout` deletes it)",
        sign_in_again()
    ))
}

/// `<profile dir>/session.json`.
pub fn path() -> PathBuf {
    state::profile_directory().join("session.json")
}

/// Reads the session file. Never fails on content: damaged or old files are reported.
pub fn load() -> Result<Loaded, CliError> {
    let path = path();
    match fs::read(&path) {
        Ok(bytes) => Ok(classify(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded::Missing),
        Err(e) => Err(CliError::io(
            format!("could not read {}", path.display()),
            &e,
        )),
    }
}

/// What a session file's bytes are: a current session, an old one, or neither.
fn classify(bytes: &[u8]) -> Loaded {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Loaded::Unreadable;
    };
    if value.get("version").and_then(Value::as_u64) == Some(u64::from(VERSION)) {
        // Never surface serde's message: it can quote values from the file.
        return serde_json::from_value::<StoredSession>(value)
            .map_or(Loaded::Unreadable, |s| Loaded::Session(Box::new(s)));
    }
    let legacy = value.is_object()
        && value.get("version").is_none()
        && ["org_id", "actor", "refresh_token", "access_token"]
            .iter()
            .any(|key| value.get(key).is_some());
    if legacy {
        Loaded::Legacy
    } else {
        Loaded::Unreadable
    }
}

pub fn save(session: &StoredSession) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(session)
        .map_err(|e| CliError::new("io_error", format!("could not encode the session: {e}")))?;
    state::private_write(&path(), &bytes)
}

/// Deletes the session file; false when there was none.
pub fn remove() -> Result<bool, CliError> {
    match fs::remove_file(path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CliError::io(
            format!("could not delete {}", path().display()),
            &e,
        )),
    }
}

/// An exclusive lock on the profile's session (released on drop).
pub struct Lock(#[allow(dead_code)] fs::File);

pub async fn lock() -> Result<Lock, CliError> {
    let directory = state::profile_directory();
    tokio::task::spawn_blocking(move || lock_in(&directory))
        .await
        .map_err(|e| CliError::new("io_error", format!("the session lock task failed: {e}")))?
}

/// Blocks until this process holds `<directory>/session.lock`.
fn lock_in(directory: &std::path::Path) -> Result<Lock, CliError> {
    state::private_directory(directory)?;
    let path = directory.join("session.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|e| CliError::io(format!("could not open {}", path.display()), &e))?;
    file.lock()
        .map_err(|e| CliError::io(format!("could not lock {}", path.display()), &e))?;
    Ok(Lock(file))
}

/// The Silicon Accounts client for a saved session.
pub fn accounts_auth(accounts_url: &str) -> Result<AccountsAuth, CliError> {
    AccountsAuth::builder(accounts_url)
        .telemetry(state::telemetry_enabled())
        .user_agent(format!("silicon-commit-cli/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(CliError::from)
}

/// Returns the profile's session with more than a minute left on its access token,
/// refreshing it under the lock when needed.
///
/// `rejected` is an access token the Commit API refused: the session is refreshed even if
/// it looks current, unless another command already replaced that token. `expect_uuid`
/// guards a retry: if the profile signed in as someone else meanwhile, nothing is replayed.
pub async fn fresh(
    rejected: Option<&str>,
    expect_uuid: Option<&str>,
) -> Result<StoredSession, CliError> {
    let session = load()?.require()?;
    expect_same(&session, expect_uuid)?;
    if rejected.is_none() && !session.needs_refresh() {
        return Ok(session);
    }
    let _lock = lock().await?;
    // Read again under the lock: another command may have rotated the token meanwhile.
    let mut session = load()?.require()?;
    expect_same(&session, expect_uuid)?;
    let replaced = rejected.is_some_and(|token| token != session.access_token);
    if !session.needs_refresh() && (rejected.is_none() || replaced) {
        return Ok(session);
    }
    let Some(refresh_token) = session.refresh_token.clone() else {
        return Err(CliError::new(
            "session_not_refreshable",
            format!("The saved session of {} has no refresh token, and its access token is no longer usable.", session.account.id),
        )
        .hint(sign_in_again()));
    };
    if session.refresh_expires_at.is_some_and(|end| end <= now()) {
        let end = session.refresh_expires_at.map(rfc3339).unwrap_or_default();
        return Err(end_session(
            &mut session,
            "expired",
            format!("the sign-in reached its end at {end}"),
        ));
    }
    let earlier_attempt_lost = session.refresh_started_at.is_some();
    let started = now();
    session.refresh_started_at = Some(started);
    save(&session)?;
    let auth = accounts_auth(&session.accounts_url)?;
    match auth.refresh(&refresh_token).await {
        Ok(sign_in) if sign_in.account.uuid != session.account.uuid => Err(end_session(
            &mut session,
            "account_changed",
            format!(
                "Silicon Accounts refreshed the session as another account ({}), so it was not used",
                sign_in.account.id
            ),
        )),
        Ok(sign_in) => {
            session.apply(&sign_in, started);
            save(&session)?;
            Ok(session)
        }
        Err(error) if error.is_sign_in_refused() => {
            let reason = SignInRefusal::of(&error).map_or("refused", |r| r.as_str());
            let mut message = accounts_description(&error);
            if earlier_attempt_lost {
                message.push_str(
                    " (an earlier refresh of this session got no answer; Silicon Accounts ends a sign-in whose refresh token is presented twice)",
                );
            }
            Err(end_session(&mut session, reason, message))
        }
        Err(error) => Err(CliError::from(error)
            .context("Could not refresh the Commit session at Silicon Accounts")
            .hint("The saved session is kept; retry when Silicon Accounts is reachable.")),
    }
}

/// Records that the session ended and returns the error saying so.
fn end_session(session: &mut StoredSession, reason: &str, message: String) -> CliError {
    session.refresh_started_at = None;
    session.ended = Some(Ended {
        at: now(),
        reason: reason.to_owned(),
        message,
    });
    if let Err(error) = save(session) {
        return error;
    }
    session.ended_error()
}

fn accounts_description(error: &ClientError) -> String {
    match error {
        ClientError::Accounts(inner) => inner.message(),
        other => other.to_string(),
    }
}

fn expect_same(session: &StoredSession, expect_uuid: Option<&str>) -> Result<(), CliError> {
    match expect_uuid {
        Some(uuid) if uuid != session.account.uuid => Err(CliError::new(
            "profile_changed",
            format!(
                "Profile `{}` signed in as {} while this command was running, so the command was not repeated as that account.",
                state::profile(),
                session.account.id
            ),
        )
        .hint("Run the command again if it should run as the new account.")),
        _ => Ok(()),
    }
}

/// Saves a new sign-in for the profile (replacing any previous one) and ends the previous
/// sign-in at Silicon Accounts when it was another sign-in. Returns a warning when ending
/// the previous one failed.
pub async fn replace(new: &StoredSession) -> Result<Option<String>, CliError> {
    let _lock = lock().await?;
    let previous = match load()? {
        Loaded::Session(previous) => Some(previous),
        _ => None,
    };
    save(new)?;
    let Some(previous) = previous else {
        return Ok(None);
    };
    if previous.ended.is_some() {
        return Ok(None);
    }
    let Some(token) = previous
        .refresh_token
        .clone()
        .or_else(|| Some(previous.access_token.clone()))
    else {
        return Ok(None);
    };
    if Some(&token) == new.refresh_token.as_ref() {
        return Ok(None);
    }
    let result = match accounts_auth(&previous.accounts_url) {
        Ok(auth) => auth
            .revoke(&token)
            .await
            .map(|_| ())
            .map_err(CliError::from),
        Err(error) => Err(error),
    };
    Ok(result.err().map(|error| {
        format!(
            "The new sign-in is saved, but ending the previous one ({}) failed: {} It stays active until it expires or is ended on the account site.",
            previous.account.id, error.message
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("commit-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn current() -> Value {
        json!({"version":2,"app_id":"commit","accounts_url":"http://localhost:9590","api_url":"http://127.0.0.1:4141",
            "access_token":"eyJ.a.b","refresh_token":"sar_x","expires_at":1,"account":{"uuid":"zQo","id":"c:ada","kind":"carbon"},
            "method":"device","signed_in_at":1})
    }

    #[test]
    fn files_are_classified_without_failing() {
        assert!(matches!(
            classify(current().to_string().as_bytes()),
            Loaded::Session(_)
        ));
        let legacy = json!({"access_token":"oat","refresh_token":"ort","org_id":"tos","actor":{"type":"carbon","public_id":"c:a"}});
        assert!(matches!(
            classify(legacy.to_string().as_bytes()),
            Loaded::Legacy
        ));
        for unreadable in [
            &b"garbage"[..],
            b"[]",
            b"{}",
            b"{\"version\":2}",
            b"{\"version\":3,\"access_token\":\"x\"}",
        ] {
            assert!(
                matches!(classify(unreadable), Loaded::Unreadable),
                "{}",
                String::from_utf8_lossy(unreadable)
            );
        }
        let mut wrong_type = current();
        wrong_type["expires_at"] = json!("eyJ.secret.value");
        assert!(matches!(
            classify(wrong_type.to_string().as_bytes()),
            Loaded::Unreadable
        ));
    }

    #[test]
    fn sessions_round_trip_and_their_debug_output_hides_tokens() {
        let Loaded::Session(session) = classify(current().to_string().as_bytes()) else {
            panic!("not a session")
        };
        assert!(session.needs_refresh());
        assert_eq!(session.who(), "c:ada, a Carbon");
        assert!(!format!("{session:?}").contains("sar_x"));
        let back: Value = serde_json::to_value(&*session).unwrap();
        assert_eq!(back["refresh_token"], "sar_x");
        assert_eq!(
            session.status_json(false)["expires_at"],
            "1970-01-01T00:00:01Z"
        );
    }

    #[test]
    fn private_writes_are_atomic_and_owner_only() {
        let dir = scratch("write");
        let file = dir.join("nested/session.json");
        state::private_write(&file, b"one").unwrap();
        state::private_write(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temporary files are left: {names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(file.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_lock_is_exclusive_until_dropped() {
        let dir = scratch("lock");
        let held = lock_in(&dir).unwrap();
        let other = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("session.lock"))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(other.try_lock().is_ok());
        let _ = fs::remove_dir_all(dir);
    }
}
