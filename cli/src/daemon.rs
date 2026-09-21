//! Compatibility commands for retiring the old updater. Honeycomb owns updates.
use super::*;
use std::process::Command as Process;
#[derive(Subcommand, Clone)]
pub enum DaemonCommand {
    /// Explain how to install Commit through Honeycomb.
    Install,
    /// Remove the legacy user updater, preserving saved sessions.
    Uninstall,
    /// Show Honeycomb update ownership and any legacy service file.
    Status,
    /// Compatibility command; never downloads or replaces the CLI.
    Run {
        #[arg(long)]
        once: bool,
    },
}
pub const fn updates_enabled() -> bool {
    false
}
pub fn configure_updates(enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    if enabled {
        return Err("Honeycomb manages Commit updates. Use honeycomb install 'tos>commit'; configure updates in Honeycomb.".into());
    }
    runtime::private_write(&runtime::directory().join("auto-update"), b"off")?;
    println!("Commit self-updates are disabled. Honeycomb manages installation and updates.");
    Ok(())
}
pub async fn command(command: &DaemonCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        DaemonCommand::Uninstall => uninstall(),
        DaemonCommand::Status => {
            let service = service_path().ok();
            println!("{}", serde_json::json!({"auto_update":false,"update_manager":"honeycomb","installed":service.as_ref().is_some_and(|p|p.exists()),"service_file":service}));
            Ok(())
        }
        DaemonCommand::Install | DaemonCommand::Run { .. } => Err("Honeycomb manages Commit updates. Use honeycomb install 'tos>commit'. Remove an old updater with commit daemon uninstall.".into()),
    }
}
fn user_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").ok_or("HOME is required to install a user service")?,
    ))
}
fn service_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::consts::OS {
        "macos"=>Ok(user_home()?.join("Library/LaunchAgents/com.teamofsilicons.commit-updater.plist")),
        "linux"=>Ok(user_home()?.join(".config/systemd/user/silicon-commit-updater.service")),
        _=>Err("legacy updater removal supports macOS and Linux; manage installation through Honeycomb".into())
    }
}
fn checked(program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Process::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} could not configure the user service; check its error above").into())
    }
}
fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let service = service_path()?;
    if std::env::consts::OS == "macos" {
        let uid = Process::new("id").arg("-u").output()?;
        let _ = Process::new("launchctl")
            .args([
                "bootout",
                &format!(
                    "gui/{}/com.teamofsilicons.commit-updater",
                    String::from_utf8(uid.stdout)?.trim()
                ),
            ])
            .output();
    } else {
        checked(
            "systemctl",
            &["--user", "disable", "--now", "silicon-commit-updater.timer"],
        )?;
        if service.with_extension("timer").exists() {
            fs::remove_file(service.with_extension("timer"))?;
        }
    }
    if service.exists() {
        fs::remove_file(service)?;
    }
    println!("Hourly updater removed.");
    Ok(())
}
