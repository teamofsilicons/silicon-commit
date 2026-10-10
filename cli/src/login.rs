//! `commit login`, `commit login status`, `commit logout`, `commit accounts`.

use std::io::{IsTerminal as _, Read as _};

use serde_json::{Value, json};
use silicon_commit_client::auth::{
    APP_ID, AccountLink, AccountsAuth, DEFAULT_ACCOUNTS_URL, DeviceEvent, SignIn, peek_claims,
};
use silicon_commit_client::{DEFAULT_API_URL, Error as ClientError, ExposeSecret as _};

use crate::output::{CARBON_LOGIN, CliError, SILICON_LOGIN, now, print_json, rfc3339};
use crate::session::{self, Loaded, StoredSession};
use crate::{LoginArgs, LogoutArgs, Root, StatusArgs, api, docs, state};

pub const LOGIN_HELP: &str = "\
Carbons sign in with a code: `commit login` prints a link and a code, you approve it on
the account site (signed in as yourself), and the CLI saves the session. Add --open to open
the link in a browser, --scope email to share your email with Commit (for email
notifications), --json for JSON progress lines on stderr.

Silicons never see a page: mint a short-lived token for Commit and hand it over on stdin.
  silicon-accounts login --app commit -q | commit login --slt-stdin
  commit login --slt slt_…            (or the positional form: commit login slt_…)
A short-lived token works once, for 2 minutes, only for Commit.

The session is saved in the profile's directory ($SILICON_HOME/.commit/session.json, mode
0600) and refreshed automatically when less than a minute is left. Signing in again replaces
the profile's session and ends the previous sign-in. Use --profile NAME to keep several
accounts side by side. Check with `commit login status`; end it with `commit logout`.

Examples:
  commit login
  commit login --scope email --open
  silicon-accounts login --app commit -q | commit --profile scout login --slt-stdin
  ACCOUNTS_URL=http://localhost:9590 COMMIT_API_URL=http://127.0.0.1:4141 commit login";

pub const STATUS_HELP: &str = "\
Without --offline, a session about to expire is refreshed and the Commit API is asked to
confirm it (`verified`: true). If the API cannot be reached, the saved session is reported
with `verified`: false and a `warning`.

JSON, signed in:
  {\"authenticated\":true,\"uuid\":\"zQo\",\"id\":\"c:ada\",\"kind\":\"carbon\",\"display_name\":\"Ada\",
   \"expires_at\":\"…\",\"refresh_expires_at\":\"…\",\"verified\":true,\"profile\":\"default\",…}
JSON, signed out (exit 0):
  {\"authenticated\":false}   plus \"reason\" when a session exists but cannot be used
  (session_ended, legacy_session, unreadable_session, signed_in_elsewhere, token_rejected)
Without --json the exit status is 0 when signed in and 1 when not.";

/// Where requests go: explicit flag or environment, else the saved session's, else production.
pub struct Targets {
    pub api_url: String,
    pub accounts_url: String,
}

pub fn targets(root: &Root, saved: Option<&StoredSession>) -> Targets {
    let pick = |explicit: &Option<String>, saved: Option<&str>, default: &str| {
        explicit
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or(saved)
            .unwrap_or(default)
            .trim_end_matches('/')
            .to_owned()
    };
    Targets {
        api_url: pick(
            &root.api_url,
            saved.map(|s| s.api_url.as_str()),
            DEFAULT_API_URL,
        ),
        accounts_url: pick(
            &root.accounts_url,
            saved.map(|s| s.accounts_url.as_str()),
            DEFAULT_ACCOUNTS_URL,
        ),
    }
}

/// Compares API URLs given as an origin or `…/api/v1`.
pub fn same_api(a: &str, b: &str) -> bool {
    let norm = |v: &str| {
        let v = v.trim().trim_end_matches('/');
        v.strip_suffix("/api/v1").unwrap_or(v).to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

fn same_accounts(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// The saved session belongs to other servers than the ones explicitly asked for.
pub fn elsewhere(root: &Root, session: &StoredSession) -> Option<CliError> {
    let api = root.api_url.as_deref().filter(|v| !v.trim().is_empty());
    let accounts = root
        .accounts_url
        .as_deref()
        .filter(|v| !v.trim().is_empty());
    let api_differs = api.is_some_and(|v| !same_api(v, &session.api_url));
    let accounts_differs = accounts.is_some_and(|v| !same_accounts(v, &session.accounts_url));
    (api_differs || accounts_differs).then(|| {
        CliError::new(
            "signed_in_elsewhere",
            format!(
                "Profile `{}` is signed in for the Commit API at {} (Silicon Accounts at {}), not the one you asked for, so its session is not sent there.",
                state::profile(),
                session.api_url,
                session.accounts_url
            ),
        )
        .hint("Sign in for that server (`commit login` with the same --api-url/--accounts-url), use another --profile, or unset COMMIT_API_URL/ACCOUNTS_URL.")
    })
}

/// `commit accounts [--json]` (and the hidden `iam`): never fails, never touches the network.
pub fn accounts(root: &Root, _json: bool) {
    let saved = match session::load() {
        Ok(Loaded::Session(session)) => Some(*session),
        _ => None,
    };
    let targets = targets(root, saved.as_ref());
    print_json(&json!({
        "app_id": APP_ID,
        "accounts_url": targets.accounts_url,
        "api_url": targets.api_url,
        "version": env!("CARGO_PKG_VERSION"),
        "command": "commit",
        "profile": state::profile(),
        "login": {
            "carbon": CARBON_LOGIN,
            "silicon": SILICON_LOGIN,
            "status": "commit login status --json",
        },
        "docs": docs::DOCS_URL,
        "repository": docs::REPOSITORY_URL,
        "rust_client": docs::CLIENT_URL,
    }));
}

fn progress(json_mode: bool, event: &Value, text: &str) {
    if json_mode {
        eprintln!("{event}");
    } else {
        eprintln!("{text}");
    }
}

pub fn warn(json_mode: bool, text: &str) {
    if json_mode {
        eprintln!("{}", json!({ "warning": text }));
    } else {
        eprintln!("warning: {text}");
    }
}

/// The short-lived token from --slt, the positional argument, or stdin.
fn slt_input(args: &LoginArgs) -> Result<Option<String>, CliError> {
    if args.slt_stdin {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err(CliError::new(
                "stdin_is_terminal",
                "--slt-stdin reads the short-lived token from standard input, but standard input is a terminal.",
            )
            .hint(format!("Pipe it in: `{SILICON_LOGIN}`.")));
        }
        let mut text = String::new();
        stdin
            .by_ref()
            .take(64 * 1024)
            .read_to_string(&mut text)
            .map_err(|e| {
                CliError::io(
                    "could not read the short-lived token from standard input",
                    &e,
                )
            })?;
        let token = text.trim();
        if token.is_empty() {
            return Err(CliError::new(
                "empty_slt",
                "Standard input was empty, so there is no short-lived token to exchange.",
            )
            .hint(format!("Mint one and pipe it: `{SILICON_LOGIN}`.")));
        }
        return Ok(Some(token.to_owned()));
    }
    if let Some(value) = &args.positional_slt {
        let word = value.trim();
        if !word.starts_with("slt_")
            && word.len() < 24
            && word.bytes().all(|b| b.is_ascii_lowercase())
        {
            return Err(CliError::new(
                "unknown_login_argument",
                format!("`{word}` is neither a subcommand of `commit login` nor a short-lived token (those start with slt_)."),
            )
            .hint("Subcommand: `commit login status`. Carbons: `commit login`. Silicons: `silicon-accounts login --app commit -q | commit login --slt-stdin`."));
        }
    }
    Ok(args.slt.clone().or_else(|| args.positional_slt.clone()))
}

/// `commit login`: device flow (Carbons) or short-lived token exchange (Silicons).
pub async fn login(root: &Root, args: &LoginArgs) -> Result<(), CliError> {
    let slt = slt_input(args)?;
    if slt.is_some() && (args.scope.is_some() || args.open) {
        return Err(CliError::new(
            "conflicting_login_options",
            "--scope and --open belong to the device sign-in; a short-lived token already carries the details chosen when it was minted.",
        )
        .hint("Drop --scope/--open, or sign in with `commit login` (no token) to use them."));
    }
    let saved = match session::load()? {
        Loaded::Session(previous) => Some(*previous),
        _ => None,
    };
    let targets = targets(root, saved.as_ref());
    api::client(&targets.api_url, None)?;
    let auth = session::accounts_auth(&targets.accounts_url)?;
    let started = now();
    let (sign_in, method) = match &slt {
        Some(token) => (
            auth.exchange_slt(token).await.map_err(CliError::from)?,
            "slt",
        ),
        None => (device(&auth, args).await?, "device"),
    };
    let session = StoredSession::new(
        &sign_in,
        &targets.accounts_url,
        &targets.api_url,
        method,
        started,
    );
    if args.no_save {
        warn(
            args.json,
            "--no-save prints secret tokens: keep this output private and never log it.",
        );
        print_json(&json!({
            "access_token": sign_in.access_token.expose_secret(),
            "refresh_token": sign_in.refresh_token.as_ref().map(|t| t.expose_secret().to_owned()),
            "expires_at": rfc3339(session.expires_at),
            "refresh_expires_at": session.refresh_expires_at.map(rfc3339),
            "scope": sign_in.scope,
            "account": sign_in.account,
            "app_id": APP_ID,
            "accounts_url": session.accounts_url,
            "api_url": session.api_url,
        }));
        return Ok(());
    }
    if let Some(warning) = session::replace(&session).await? {
        warn(args.json, &warning);
    }
    let verified = match api::client(&session.api_url, Some(&session.access_token))?
        .me()
        .await
    {
        Ok(_) => true,
        Err(error) => {
            warn(
                args.json,
                &format!(
                    "Signed in, but the Commit API at {} did not confirm the session: {error}",
                    session.api_url
                ),
            );
            false
        }
    };
    let mut result = session.status_json(verified);
    result["method"] = json!(method);
    if args.json {
        print_json(&result);
    } else {
        println!("{}", signed_in_text(&session, verified));
        println!("Next: commit todos list --view assigned_to_me   (explore: commit --help)");
    }
    Ok(())
}

async fn device(auth: &AccountsAuth, args: &LoginArgs) -> Result<SignIn, CliError> {
    let label = format!("Commit CLI on {}", std::env::consts::OS);
    let device = auth
        .start_device_sign_in(args.scope.as_deref(), Some(&label))
        .await
        .map_err(|e| CliError::from(e).context("Silicon Accounts did not start the sign-in"))?;
    let expires_at = now().saturating_add(i64::try_from(device.expires_in).unwrap_or(600));
    let opened = args.open && open_browser(device.browser_url());
    let note = if opened {
        "Your browser was opened; approve the code there.".to_owned()
    } else {
        format!(
            "Or open {} directly. Approve it while signed in to the account site as yourself.",
            device.browser_url()
        )
    };
    progress(
        args.json,
        &json!({
            "event": "device_code",
            "user_code": device.user_code,
            "verification_uri": device.verification_uri,
            "verification_uri_complete": device.verification_uri_complete,
            "expires_in": device.expires_in,
            "expires_at": rfc3339(expires_at),
            "interval": device.interval,
            "browser_opened": opened,
        }),
        &format!(
            "To sign in to Commit, open {} and enter the code\n\n    {}\n\n{note}\nWaiting for approval (the code expires in {} minutes; Ctrl-C to cancel)…",
            device.verification_uri,
            device.user_code,
            device.expires_in.div_ceil(60).max(1)
        ),
    );
    let json_mode = args.json;
    let wait = auth.wait_for_device_sign_in(&device, |event| match event {
        DeviceEvent::SlowDown { interval } => progress(
            json_mode,
            &json!({ "event": "slow_down", "interval": interval.as_secs() }),
            &format!(
                "Silicon Accounts asked to poll more slowly; checking every {} s now.",
                interval.as_secs()
            ),
        ),
        DeviceEvent::TransientError { error, retry_in } => warn(
            json_mode,
            &format!("{error} Retrying in {} s.", retry_in.as_secs()),
        ),
        _ => {}
    });
    tokio::select! {
        result = wait => result.map_err(|error| device_error(error, &device.user_code)),
        _ = tokio::signal::ctrl_c() => {
            let mut error = CliError::new("interrupted", format!("Stopped waiting for approval of code {}.", device.user_code))
                .hint("Run `commit login` again for a new code.");
            error.exit = 130;
            Err(error)
        }
    }
}

fn device_error(error: ClientError, user_code: &str) -> CliError {
    let code = error.code().to_owned();
    let cli = CliError::from(error);
    match code.as_str() {
        "access_denied" => cli.hint("Run `commit login` again if that was a mistake."),
        "expired_token" => cli.hint(format!(
            "Run `commit login` again for a new code, and approve it within 10 minutes (this one was {user_code})."
        )),
        "unauthorized_client" => cli.hint(
            "Commit's sign-in setup at Silicon Accounts does not allow device sign-in; Silicons can still use short-lived tokens. Report it with `commit report`.",
        ),
        _ => cli,
    }
}

fn open_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn signed_in_text(session: &StoredSession, verified: bool) -> String {
    let mut text = format!("Signed in to Commit as {}.", session.who());
    if let Some(custodian) = &session.account.custodian {
        text.push_str(&format!(" Custodian: {}.", custodian.id));
    }
    let rows = [
        ("uuid", session.account.uuid.clone()),
        ("profile", state::profile().to_owned()),
        ("api", session.api_url.clone()),
        ("accounts", session.accounts_url.clone()),
        (
            "access token",
            format!(
                "until {} (refreshed automatically)",
                rfc3339(session.expires_at)
            ),
        ),
        (
            "session ends",
            session
                .refresh_expires_at
                .map_or_else(|| "unknown".to_owned(), rfc3339),
        ),
        (
            "verified",
            if verified {
                "yes, the Commit API accepted it"
            } else {
                "no, the Commit API was not reached"
            }
            .to_owned(),
        ),
    ];
    for (key, value) in rows {
        text.push_str(&format!("\n{key:<14}{value}"));
    }
    text
}

fn signed_out(reason: &str, message: &str) -> Value {
    json!({ "authenticated": false, "reason": reason, "message": message })
}

/// `commit login status`: exit 0 with `--json`; otherwise 0 signed in, 1 signed out.
pub async fn status(root: &Root, args: &StatusArgs) -> Result<u8, CliError> {
    let value = match &root.token {
        Some(token) => token_status(root, token, args.offline).await?,
        None => session_status(root, args.offline).await?,
    };
    let authenticated = value["authenticated"] == true;
    if args.json {
        print_json(&value);
        return Ok(0);
    }
    if authenticated {
        let id = value["id"].as_str().unwrap_or("");
        let kind = match value["kind"].as_str() {
            Some("carbon") => "Carbon",
            Some("silicon") => "Silicon",
            _ => "account",
        };
        let name = value["display_name"]
            .as_str()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        println!("Signed in to Commit as {id}{name}, a {kind}.");
        for key in [
            "uuid",
            "profile",
            "api_url",
            "accounts_url",
            "expires_at",
            "refresh_expires_at",
            "verified",
            "warning",
        ] {
            if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
                println!(
                    "{key:<20}{}",
                    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                );
            }
        }
    } else {
        let why = value["message"]
            .as_str()
            .map(|m| format!(" {m}"))
            .unwrap_or_default();
        println!(
            "Not signed in to Commit on profile `{}`.{why}",
            state::profile()
        );
        println!("Carbon: {CARBON_LOGIN}\nSilicon: {SILICON_LOGIN}");
    }
    Ok(u8::from(!authenticated))
}

async fn token_status(root: &Root, token: &str, offline: bool) -> Result<Value, CliError> {
    let Some(claims) = peek_claims(token) else {
        return Ok(signed_out(
            "token_malformed",
            "The token from --token/COMMIT_ACCESS_TOKEN is not a Silicon Accounts access token (not a JWT).",
        ));
    };
    if claims.exp <= now() {
        return Ok(signed_out(
            "token_expired",
            &format!(
                "The token from --token/COMMIT_ACCESS_TOKEN expired at {}.",
                rfc3339(claims.exp)
            ),
        ));
    }
    let api_url = targets(root, None).api_url;
    let mut value = json!({
        "authenticated": true,
        "uuid": claims.sub,
        "id": claims.id,
        "kind": claims.kind.map(|k| k.as_str()),
        "expires_at": rfc3339(claims.exp),
        "refresh_expires_at": null,
        "verified": false,
        "source": "token",
        "api_url": api_url,
    });
    if offline {
        return Ok(value);
    }
    match api::client(&api_url, Some(token))?.me().await {
        Ok(me) => {
            value["verified"] = json!(true);
            for key in ["id", "kind", "display_name"] {
                if let Some(v) = me
                    .get(key)
                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                {
                    value[key] = v.clone();
                }
            }
            Ok(value)
        }
        Err(error) if error.is_unauthenticated() => {
            let code = error.code().to_owned();
            Ok(signed_out(&code, &CliError::from(error).message))
        }
        Err(error) => {
            value["warning"] = json!(format!("The Commit API did not confirm the token: {error}"));
            Ok(value)
        }
    }
}

async fn session_status(root: &Root, offline: bool) -> Result<Value, CliError> {
    let saved = match session::load()? {
        Loaded::Missing => return Ok(json!({ "authenticated": false })),
        Loaded::Legacy => {
            return Ok(signed_out(
                "legacy_session",
                &session::legacy_error().message,
            ));
        }
        Loaded::Unreadable => {
            return Ok(signed_out(
                "unreadable_session",
                &session::unreadable_error().message,
            ));
        }
        Loaded::Session(saved) => *saved,
    };
    if saved.ended.is_some() {
        return Ok(signed_out("session_ended", &saved.ended_error().message));
    }
    if elsewhere(root, &saved).is_some() {
        return Ok(json!({
            "authenticated": false,
            "reason": "signed_in_elsewhere",
            "message": format!("Profile `{}` is signed in for another Commit API or Silicon Accounts.", state::profile()),
            "session_api_url": saved.api_url,
            "session_accounts_url": saved.accounts_url,
        }));
    }
    if offline {
        if saved.refresh_expires_at.is_some_and(|end| end <= now()) {
            return Ok(signed_out(
                "session_ended",
                "The saved sign-in reached its end; sign in again.",
            ));
        }
        return Ok(saved.status_json(false));
    }
    let current = match session::fresh(None, None).await {
        Ok(current) => current,
        Err(error) if error.code == "session_ended" => {
            return Ok(signed_out("session_ended", &error.message));
        }
        Err(error) => {
            let mut value = saved.status_json(false);
            value["warning"] = json!(error.message);
            return Ok(value);
        }
    };
    let first = verify(&current).await;
    let (current, outcome) = match first {
        Err(error) if error.is_unauthenticated() => {
            match session::fresh(Some(&current.access_token), Some(&current.account.uuid)).await {
                Ok(renewed) => {
                    let outcome = verify(&renewed).await;
                    (renewed, outcome)
                }
                Err(error) if error.code == "session_ended" => {
                    return Ok(signed_out("session_ended", &error.message));
                }
                Err(error) => {
                    let mut value = current.status_json(false);
                    value["warning"] = json!(error.message);
                    return Ok(value);
                }
            }
        }
        other => (current, other),
    };
    match outcome {
        Ok(me) => {
            let current = remember_account(current, &me).await;
            Ok(current.status_json(true))
        }
        Err(error) if error.is_unauthenticated() => {
            let code = error.code().to_owned();
            Ok(signed_out(&code, &CliError::from(error).message))
        }
        Err(error) => {
            let mut value = current.status_json(false);
            value["warning"] = json!(format!(
                "The Commit API did not confirm the session: {error}"
            ));
            Ok(value)
        }
    }
}

async fn verify(session: &StoredSession) -> Result<Value, ClientError> {
    api::client(&session.api_url, Some(&session.access_token))
        .map_err(|e| ClientError::Invalid(e.message.clone()))?
        .me()
        .await
}

/// Stores the id, name and custodian the Commit API reported (they can change).
async fn remember_account(mut current: StoredSession, me: &Value) -> StoredSession {
    let text = |key: &str| me.get(key).and_then(Value::as_str).map(str::to_owned);
    let custodian = me.get("custodian").and_then(|c| {
        Some(AccountLink {
            uuid: c.get("uuid")?.as_str()?.to_owned(),
            id: c
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    });
    let id = text("id")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| current.account.id.clone());
    let name = text("display_name").unwrap_or_else(|| current.account.display_name.clone());
    let custodian = custodian.or_else(|| current.account.custodian.clone());
    if id == current.account.id
        && name == current.account.display_name
        && custodian == current.account.custodian
    {
        return current;
    }
    let Ok(_lock) = session::lock().await else {
        return current;
    };
    if let Ok(Loaded::Session(mut stored)) = session::load()
        && stored.account.uuid == current.account.uuid
        && stored.ended.is_none()
    {
        stored.account.id.clone_from(&id);
        stored.account.display_name.clone_from(&name);
        stored.account.custodian.clone_from(&custodian);
        if session::save(&stored).is_ok() {
            current = *stored;
        }
    }
    current
}

/// `commit logout`: ends the sign-in at Silicon Accounts, then deletes the saved session.
pub async fn logout(args: &LogoutArgs) -> Result<(), CliError> {
    let report = |value: Value, text: &str| {
        if args.json {
            print_json(&value);
        } else {
            println!("{text}");
        }
    };
    if !session::path().exists() {
        report(
            json!({ "signed_out": false, "reason": "not_signed_in" }),
            &format!(
                "Profile `{}` was not signed in; nothing to do.",
                state::profile()
            ),
        );
        return Ok(());
    }
    let _lock = session::lock().await?;
    let stored = match session::load()? {
        Loaded::Missing => {
            report(
                json!({ "signed_out": false, "reason": "not_signed_in" }),
                "Not signed in; nothing to do.",
            );
            return Ok(());
        }
        Loaded::Legacy | Loaded::Unreadable => {
            session::remove()?;
            report(
                json!({ "signed_out": true, "revoked": false, "reason": "unusable_session" }),
                "Deleted a saved session that could not be used (from the previous sign-in system, or damaged).",
            );
            return Ok(());
        }
        Loaded::Session(stored) => *stored,
    };
    if stored.ended.is_some() {
        session::remove()?;
        report(
            json!({ "signed_out": true, "id": stored.account.id, "uuid": stored.account.uuid, "revoked": false, "reason": "session_ended" }),
            &format!(
                "Deleted the saved session of {}; it had already ended.",
                stored.account.id
            ),
        );
        return Ok(());
    }
    let token = stored
        .refresh_token
        .clone()
        .unwrap_or_else(|| stored.access_token.clone());
    let revocation = match session::accounts_auth(&stored.accounts_url) {
        Ok(auth) => auth.revoke(&token).await.map_err(CliError::from),
        Err(error) => Err(error),
    };
    match revocation {
        Ok(outcome) => {
            session::remove()?;
            let mut value = json!({ "signed_out": true, "id": stored.account.id, "uuid": stored.account.uuid, "revoked": outcome.revoked });
            if let Some(message) = &outcome.message {
                value["message"] = json!(message);
            }
            let text = if outcome.revoked {
                format!("Signed out of Commit as {}; the sign-in was ended at Silicon Accounts.", stored.who())
            } else {
                format!(
                    "Signed out of Commit as {}; Silicon Accounts had nothing to end: {}",
                    stored.who(),
                    outcome.message.as_deref().unwrap_or("the sign-in was already over.")
                )
            };
            report(value, &text);
            Ok(())
        }
        Err(error) if args.force => {
            session::remove()?;
            let warning = format!(
                "Silicon Accounts did not end the sign-in ({}); the local session is deleted anyway, and the sign-in stays active until it expires or is ended on the account site.",
                error.message
            );
            warn(args.json, &warning);
            report(
                json!({ "signed_out": true, "id": stored.account.id, "uuid": stored.account.uuid, "revoked": false, "warning": warning }),
                &format!("Deleted the saved session of {}.", stored.account.id),
            );
            Ok(())
        }
        Err(error) => Err(error
            .context("Could not end the sign-in at Silicon Accounts")
            .hint("The saved session is kept so `commit logout` can be retried. `commit logout --force` deletes it locally and leaves the sign-in active until it expires or is ended on the account site.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_urls_compare_by_origin_with_or_without_the_api_path() {
        assert!(same_api(
            "https://backend.commit.teamofsilicons.com",
            "https://backend.commit.teamofsilicons.com/api/v1/"
        ));
        assert!(same_api(
            "http://127.0.0.1:4141/",
            "http://127.0.0.1:4141/api/v1"
        ));
        assert!(!same_api("http://127.0.0.1:4141", "http://127.0.0.1:4142"));
        assert!(same_accounts(
            "http://localhost:9590/",
            "http://localhost:9590"
        ));
        assert!(!same_accounts(
            "http://localhost:9590",
            "https://accounts.teamofsilicons.com"
        ));
    }
}
