//! Where Commit keeps local state: `$SILICON_HOME/.commit` (or `~/.commit`), an optional
//! home-directory pointer (`commit config home`), named profiles, and private file writes.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::CliError;

static PROFILE: OnceLock<String> = OnceLock::new();

/// Selects the profile for this process (called once from `main`).
pub fn set_profile(profile: String) {
    let _ = PROFILE.set(profile);
}

/// The selected profile (`default` unless `--profile`/`COMMIT_PROFILE` chose another).
pub fn profile() -> &'static str {
    PROFILE.get().map_or("default", String::as_str)
}

/// Validates a profile name for clap.
pub fn profile_name(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
    {
        return Err("profile names are 1 to 64 lowercase letters, digits, `_` or `-`".into());
    }
    Ok(value.to_owned())
}

/// `$SILICON_HOME`, else `$HOME`, else the current directory.
pub fn default_home() -> PathBuf {
    std::env::var_os("SILICON_HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// The file that remembers `commit config home`.
pub fn home_pointer() -> PathBuf {
    default_home().join(".commit").join("home_dir")
}

/// The home chosen with `commit config home`, if any.
pub fn configured_home() -> Option<PathBuf> {
    let text = fs::read_to_string(home_pointer()).ok()?;
    let path = PathBuf::from(text.trim());
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The effective home: the configured one, else the default.
pub fn home() -> PathBuf {
    configured_home().unwrap_or_else(default_home)
}

/// `<home>/.commit`: telemetry setting, saved reports, the default profile's session.
pub fn directory() -> PathBuf {
    home().join(".commit")
}

/// The selected profile's directory: `<home>/.commit` or `<home>/.commit/profiles/<name>`.
pub fn profile_directory() -> PathBuf {
    if profile() == "default" {
        directory()
    } else {
        directory().join("profiles").join(profile())
    }
}

/// `commit config home LOCATION`.
pub fn set_home(location: &Path) -> Result<PathBuf, CliError> {
    let location = expand_tilde(location);
    if !location.is_dir() {
        return Err(CliError::new(
            "not_a_directory",
            format!("not a directory: {}", location.display()),
        )
        .hint("Create the directory first, or pass an existing one; Commit keeps its state in <LOCATION>/.commit."));
    }
    let location = fs::canonicalize(&location)
        .map_err(|e| CliError::io(format!("could not resolve {}", location.display()), &e))?;
    private_write(&home_pointer(), location.to_string_lossy().as_bytes())?;
    Ok(location)
}

fn expand_tilde(location: &Path) -> PathBuf {
    match location.to_str() {
        Some("~") => default_home(),
        Some(text) if text.starts_with("~/") => default_home().join(&text[2..]),
        _ => location.to_path_buf(),
    }
}

/// Whether diagnostics are on: `COMMIT_TELEMETRY`, else `commit config telemetry`, else on.
pub fn telemetry_enabled() -> bool {
    std::env::var("COMMIT_TELEMETRY")
        .ok()
        .or_else(|| fs::read_to_string(directory().join("telemetry")).ok())
        .is_none_or(|v| !matches!(v.trim(), "off" | "false" | "0"))
}

/// Creates `parent` (mode 0700 on Unix) if needed.
pub fn private_directory(parent: &Path) -> Result<(), CliError> {
    fs::create_dir_all(parent)
        .map_err(|e| CliError::io(format!("could not create {}", parent.display()), &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| CliError::io(format!("could not protect {}", parent.display()), &e))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically (temporary file, fsync, rename) with mode 0600.
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let parent = path.parent().ok_or_else(|| {
        CliError::new(
            "invalid_state_path",
            format!("{} has no parent directory", path.display()),
        )
    })?;
    private_directory(parent)?;
    let file_name = path
        .file_name()
        .map_or_else(|| "state".into(), |n| n.to_string_lossy());
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(CliError::io(
            format!("could not write {}", path.display()),
            &error,
        ));
    }
    Ok(())
}
