//! Errors and output. Results go to stdout (JSON for API commands); progress, warnings and
//! errors go to stderr. Every error says what failed, why, and what to do next.

use serde_json::{Value, json};
use silicon_commit_client::Error as ClientError;
use silicon_commit_client::auth::SignInRefusal;

/// How a Silicon signs in, quoted in hints.
pub const SILICON_LOGIN: &str = "silicon-accounts login --app commit -q | commit login --slt-stdin";
/// How a Carbon signs in, quoted in hints.
pub const CARBON_LOGIN: &str = "commit login";

/// A failed command (boxed: errors travel through many `Result`s).
#[derive(Debug)]
pub struct CliError(Box<ErrorData>);

/// What a failed command reports.
#[derive(Debug)]
pub struct ErrorData {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub details: Option<Value>,
    pub reason: Option<String>,
    pub exit: u8,
}

impl std::ops::Deref for CliError {
    type Target = ErrorData;
    fn deref(&self) -> &ErrorData {
        &self.0
    }
}

impl std::ops::DerefMut for CliError {
    fn deref_mut(&mut self) -> &mut ErrorData {
        &mut self.0
    }
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::from_data(ErrorData {
            code: code.into(),
            message: message.into(),
            hint: None,
            status: None,
            request_id: None,
            details: None,
            reason: None,
            exit: 1,
        })
    }

    fn from_data(data: ErrorData) -> Self {
        Self(Box::new(data))
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn io(context: impl Into<String>, error: &std::io::Error) -> Self {
        Self::new("io_error", format!("{}: {error}", context.into()))
    }

    /// Not signed in on this profile.
    pub fn not_signed_in() -> Self {
        Self::new(
            "not_signed_in",
            format!("Not signed in to Commit on profile `{}`.", crate::state::profile()),
        )
        .hint(format!(
            "Carbon: `{CARBON_LOGIN}`. Silicon: `{SILICON_LOGIN}`. Check with `commit login status`. Or pass an access token with --token."
        ))
    }

    /// Prefixes the message with what was being done.
    #[must_use]
    pub fn context(mut self, what: &str) -> Self {
        self.message = format!("{what}: {}", self.message);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        for (key, value) in [
            ("hint", self.hint.clone().map(Value::from)),
            ("status", self.status.map(Value::from)),
            ("request_id", self.request_id.clone().map(Value::from)),
            ("reason", self.reason.clone().map(Value::from)),
            ("details", self.details.clone()),
        ] {
            if let Some(value) = value {
                error[key] = value;
            }
        }
        json!({ "error": error })
    }

    /// Prints the error on stderr: one JSON line in JSON mode, readable text otherwise.
    pub fn print(&self, json_mode: bool) {
        if json_mode {
            eprintln!("{}", self.to_json());
            return;
        }
        let mut meta = Vec::new();
        if let Some(status) = self.status {
            meta.push(format!("HTTP {status}"));
        }
        meta.push(self.code.clone());
        if let Some(id) = &self.request_id {
            meta.push(format!("request ID {id}"));
        }
        eprintln!("commit: {} ({})", self.message, meta.join(", "));
        if let Some(details) = &self.details {
            eprintln!("  details: {details}");
        }
        if let Some(hint) = &self.hint {
            eprintln!("  hint: {hint}");
        }
    }
}

impl From<ClientError> for CliError {
    fn from(error: ClientError) -> Self {
        if let Some(refusal) = SignInRefusal::of(&error) {
            return Self::from_data(ErrorData {
                code: error.code().to_owned(),
                message: error
                    .as_api()
                    .map(|a| a.message.clone())
                    .unwrap_or_else(|| accounts_message(&error)),
                hint: Some(format!(
                    "Sign in again. Carbon: `{CARBON_LOGIN}`. Silicon: mint a fresh short-lived token and pass it: `{SILICON_LOGIN}`."
                )),
                status: error.status(),
                request_id: error.request_id().map(str::to_owned),
                details: None,
                reason: Some(refusal.as_str().to_owned()),
                exit: 1,
            });
        }
        match &error {
            ClientError::Api(api) => Self::from_data(ErrorData {
                code: api.code.clone(),
                message: api.message.clone(),
                hint: api
                    .hint
                    .clone()
                    .or_else(|| default_api_hint(api.status, &api.code)),
                status: Some(api.status),
                request_id: api.request_id.clone(),
                details: api.details.clone(),
                reason: None,
                exit: 1,
            }),
            ClientError::Accounts(inner) => Self::from_data(ErrorData {
                code: inner.code().to_owned(),
                message: inner.message(),
                hint: inner.hint(),
                status: inner.status(),
                request_id: inner.request_id().map(str::to_owned),
                details: inner.details().cloned(),
                reason: None,
                exit: 1,
            }),
            ClientError::Transport { .. } => Self::new(error.code(), error.to_string())
                .hint("Check --api-url (COMMIT_API_URL) and your network, then retry; a write may or may not have been applied, so retry it with the same --idempotency-key."),
            _ => Self::new(error.code(), error.to_string()),
        }
    }
}

fn accounts_message(error: &ClientError) -> String {
    match error {
        ClientError::Accounts(inner) => inner.message(),
        other => other.to_string(),
    }
}

fn default_api_hint(status: u16, code: &str) -> Option<String> {
    Some(match (status, code) {
        (401, _) => format!(
            "The Commit API refused the credential. Sign in again (`{CARBON_LOGIN}`, or `{SILICON_LOGIN}`), or pass a current --token."
        ),
        (403, "silicon_not_reachable") => {
            "That Silicon takes work only from its custodian, the custodian's other Silicons, and accounts it allowed: ask it (or its custodian) to run `commit silicons allow <silicon> <your id>`.".to_owned()
        }
        (403, _) => "You can see this but not change it; ask its owner (or a member) to make the change or share it with you.".to_owned(),
        (404, _) => "Check the identifier; it may be mistyped, deleted, or not shared with you.".to_owned(),
        (406, _) => "This CLI and the Commit API speak different contracts; update the CLI (`silicon-apps install commit`).".to_owned(),
        (409, _) => "The resource changed meanwhile; read it again and retry with the current version (--if-match).".to_owned(),
        (422, _) => "Fix the fields named in the details; `commit <command> --help` lists each command's fields.".to_owned(),
        (428, _) => "This update needs --if-match VERSION; read the resource first to get its version.".to_owned(),
        (429, _) => "Too many requests; wait and retry with the same --idempotency-key.".to_owned(),
        (500..=599, _) => "A problem on Commit's side; retry shortly, and send `commit report` with the request ID if it persists.".to_owned(),
        _ => return None,
    })
}

/// Unix seconds as RFC 3339 (UTC), e.g. `2026-10-10T08:00:00Z`.
pub fn rfc3339(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| seconds.to_string())
}

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Prints a JSON value on stdout (pretty, like every Commit command).
pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}
