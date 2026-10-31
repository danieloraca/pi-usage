//! Explicit, user-invoked launchd lifecycle. Never called by serve/export/sync.
use crate::{
    snapshot,
    sync::{self, Config},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const LABEL: &str = "com.tokengrove.sync";
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn plist(config: &Path, binary: &Path) -> String {
    let schedule = (0..60)
        .step_by(5)
        .map(|minute| format!("<dict><key>Minute</key><integer>{minute}</integer></dict>"))
        .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{}</string><string>sync</string><string>--config</string><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>StartCalendarInterval</key><array>{schedule}</array>
<key>ThrottleInterval</key><integer>60</integer>
<key>ProcessType</key><string>Background</string>
<key>Nice</key><integer>10</integer>
<key>LowPriorityIO</key><true/>
<key>ExitTimeOut</key><integer>10</integer>
</dict></plist>
"#,
        xml(&binary.to_string_lossy()),
        xml(&config.to_string_lossy())
    )
}
fn paths() -> Result<(PathBuf, PathBuf, String), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let binary = home.join("Library/Application Support/Token Grove/bin/pi-usage");
    let plist = home.join("Library/LaunchAgents/com.tokengrove.sync.plist");
    Ok((binary, plist, format!("gui/{}", unsafe { libc::geteuid() })))
}
pub fn cli(args: &[String]) -> Result<(), String> {
    let uninstall = args.first().is_some_and(|s| s == "uninstall");
    if !uninstall && (args.len() != 3 || args[1] != "--config") {
        return Err(
            "Use sync-agent render|install|uninstall|status --config /absolute/private/sync.json"
                .into(),
        );
    }
    let path = args.get(2).map(PathBuf::from).unwrap_or_default();
    if !uninstall && !path.is_absolute() {
        return Err("Config path must be absolute".into());
    }
    let config = if uninstall {
        None
    } else {
        Some(Config::read(&path)?)
    };
    let (binary, plist_path, domain) = paths()?;
    let target = format!("{domain}/{LABEL}");
    match args[0].as_str() {
        "render" => {
            print!("{}", plist(&path, &binary));
        }
        "status" => {
            let active = cfg!(target_os = "macos")
                && Command::new("/bin/launchctl")
                    .args(["print", &target])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success());
            println!("LaunchAgent loaded: {active}");
            let status = config.as_ref().unwrap().state_dir.join("status.json");
            if status.exists() {
                use std::io::Read;
                let mut text = String::new();
                snapshot::private_file(&status)?
                    .take(64 * 1024)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                println!("{text}");
            } else {
                println!("No sync attempt recorded.");
            }
        }
        "install" | "uninstall" => {
            if !cfg!(target_os = "macos") {
                return Err("LaunchAgent lifecycle commands require macOS".into());
            }
            if unsafe { libc::geteuid() } == 0 {
                return Err("Run as your logged-in Mac user, not root".into());
            }
            let active = Command::new("/bin/launchctl")
                .args(["print", &target])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| e.to_string())?
                .success();
            if active {
                let mut stop = Command::new("/bin/launchctl");
                stop.args(["bootout", &target])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                sync::bounded(&mut stop, Duration::from_secs(15), "launchctl bootout")?;
            }
            if args[0] == "uninstall" {
                if plist_path.exists() {
                    fs::remove_file(&plist_path).map_err(|e| e.to_string())?;
                }
                println!(
                    "Sync LaunchAgent removed. Config, private state, and installed binary retained."
                );
                return Ok(());
            }
            let config = config.as_ref().unwrap();
            snapshot::private_dir(&config.state_dir)?;
            snapshot::private_dir(binary.parent().unwrap())?;
            let bytes = fs::read(std::env::current_exe().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            snapshot::atomic_write(&binary, &bytes, 0o700)?;
            fs::create_dir_all(plist_path.parent().unwrap()).map_err(|e| e.to_string())?;
            snapshot::atomic_write(&plist_path, plist(&path, &binary).as_bytes(), 0o600)?;
            let mut start = Command::new("/bin/launchctl");
            start
                .arg("bootstrap")
                .arg(&domain)
                .arg(&plist_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            sync::bounded(
                &mut start,
                Duration::from_secs(15),
                "launchctl bootstrap (files installed; inspect sync status if this fails)",
            )?;
            println!(
                "Sync LaunchAgent installed: every 300 seconds and at load. Check sync status for the transfer outcome."
            );
        }
        _ => return Err("Unknown sync-agent action".into()),
    }
    Ok(())
}
