//! OS-scheduled hourly updates, independent of CLI command traffic.
use super::*;
use std::{process::Command as Process, time::Duration};
#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Register an hourly user service and run the first update check.
    Install,
    /// Remove the user service; preserves credentials and session files.
    Uninstall,
    /// Show the update setting and installed service configuration.
    Status,
    /// Run the updater; --once is used by the hourly OS service.
    Run {
        #[arg(long)]
        once: bool,
    },
}
pub fn updates_enabled() -> bool {
    fs::read_to_string(runtime::directory().join("auto-update")).map_or(true, |s| s.trim() != "off")
}
pub fn configure_updates(enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    runtime::private_write(
        &runtime::directory().join("auto-update"),
        if enabled { b"on" } else { b"off" },
    )?;
    println!(
        "Automatic updates {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}
pub async fn command(command: &DaemonCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        DaemonCommand::Install => install(),
        DaemonCommand::Uninstall => uninstall(),
        DaemonCommand::Status => {
            println!(
                "{}",
                serde_json::json!({"auto_update":updates_enabled(),"service_file":service_path()?,"installed":service_path()?.exists(),"interval_seconds":3600})
            );
            Ok(())
        }
        DaemonCommand::Run { once } => {
            loop {
                if updates_enabled()
                    && let Err(e) = update_once().await
                {
                    eprintln!(
                        "Commit update check failed: {e}; the installed CLI remains available."
                    );
                }
                if *once {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            Ok(())
        }
    }
}
async fn update_once() -> Result<(), Box<dyn std::error::Error>> {
    let directory = runtime::directory();
    fs::create_dir_all(&directory)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("update.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let release = silicon_commit_client::latest_cli_release().await?;
    let version = semver::Version::parse(&release.version)?;
    let executable = std::env::current_exe()?;
    let output = Process::new(&executable).arg("--version").output()?;
    let text = String::from_utf8(output.stdout)?;
    let installed = semver::Version::parse(
        text.split_whitespace()
            .last()
            .ok_or("cannot read installed version")?,
    )?;
    if version <= installed || !version.pre.is_empty() {
        return Ok(());
    }
    let root = executable
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("cannot locate install root")?;
    let status = Process::new("cargo")
        .args([
            "install",
            "--locked",
            "--force",
            "--version",
            &release.version,
            "--root",
        ])
        .arg(root)
        .arg("silicon-commit-cli")
        .stdout(std::process::Stdio::from(std::io::stderr()))
        .status()?;
    if !status.success() {
        return Err("cargo install failed; check daemon.log for the build error".into());
    }
    eprintln!("Installed Silicon Commit {}", release.version);
    Ok(())
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
        _=>Err("automatic service setup supports macOS and Linux; schedule commit daemon run --once hourly with your OS scheduler".into())
    }
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn systemd(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('\n', "\\n")
    )
}
fn checked(program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Process::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} could not configure the user service; check its error above").into())
    }
}
fn install() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let home = default_home_dir().to_string_lossy().into_owned();
    let path = std::env::var("PATH").unwrap_or_default();
    let log = runtime::directory().join("daemon.log");
    runtime::private_write(&log, b"")?;
    let service = service_path()?;
    if let Some(parent) = service.parent() {
        fs::create_dir_all(parent)?;
    }
    if std::env::consts::OS == "macos" {
        let uid = Process::new("id").arg("-u").output()?;
        let domain = format!("gui/{}", String::from_utf8(uid.stdout)?.trim());
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>com.teamofsilicons.commit-updater</string><key>ProgramArguments</key><array><string>{}</string><string>daemon</string><string>run</string><string>--once</string></array><key>EnvironmentVariables</key><dict><key>SILICON_HOME</key><string>{}</string><key>PATH</key><string>{}</string></dict><key>StartInterval</key><integer>3600</integer><key>RunAtLoad</key><true/><key>StandardErrorPath</key><string>{}</string></dict></plist>",
            xml(&exe),
            xml(&home),
            xml(&path),
            xml(&log.to_string_lossy())
        );
        fs::write(&service, text)?;
        let _ = Process::new("launchctl")
            .args([
                "bootout",
                &format!("{domain}/com.teamofsilicons.commit-updater"),
            ])
            .output();
        checked(
            "launchctl",
            &["bootstrap", &domain, &service.to_string_lossy()],
        )?;
    } else {
        fs::write(
            &service,
            format!(
                "[Unit]\nDescription=Silicon Commit hourly updater\n[Service]\nType=oneshot\nExecStart={} daemon run --once\nEnvironment={}\nEnvironment={}\nStandardError=append:{}\n",
                systemd(&exe),
                systemd(&format!("SILICON_HOME={home}")),
                systemd(&format!("PATH={path}")),
                log.display()
            ),
        )?;
        fs::write(
            service.with_extension("timer"),
            "[Unit]\nDescription=Check Silicon Commit updates hourly\n[Timer]\nOnStartupSec=1min\nOnUnitActiveSec=1h\nPersistent=true\n[Install]\nWantedBy=timers.target\n",
        )?;
        checked("systemctl", &["--user", "daemon-reload"])?;
        checked(
            "systemctl",
            &["--user", "enable", "--now", "silicon-commit-updater.timer"],
        )?;
    }
    println!("Hourly updater installed. Configure it with commit config updates on|off.");
    Ok(())
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
