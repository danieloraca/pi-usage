//! One bounded, non-overlapping sync iteration. Scheduling belongs to launchd.
use crate::{
    Bundle,
    snapshot::{self, Lock},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub codex_home: PathBuf,
    pub state_dir: PathBuf,
    pub ssh_target: String,
    pub remote_binary: PathBuf,
    pub remote_snapshot: PathBuf,
    #[serde(default = "export_timeout")]
    pub export_timeout_seconds: u64,
    #[serde(default = "ssh_timeout")]
    pub ssh_timeout_seconds: u64,
}
fn export_timeout() -> u64 {
    120
}
fn ssh_timeout() -> u64 {
    60
}
impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("Config path must be absolute".into());
        }
        let file = snapshot::private_file(path)?;
        let config: Self = serde_json::from_reader(file.take(64 * 1024))
            .map_err(|e| format!("Invalid sync config: {e}"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        for p in [
            &self.codex_home,
            &self.state_dir,
            &self.remote_binary,
            &self.remote_snapshot,
        ] {
            if !p.is_absolute() || p.to_string_lossy().chars().any(char::is_control) {
                return Err("Sync paths must be absolute and have no control characters".into());
            }
        }
        if self.ssh_target.is_empty()
            || self.ssh_target.starts_with('-')
            || !self
                .ssh_target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@._-".contains(&b))
        {
            return Err("ssh_target must be an SSH alias or user@host, without options".into());
        }
        if !(1..=180).contains(&self.export_timeout_seconds)
            || !(1..=90).contains(&self.ssh_timeout_seconds)
        {
            return Err(
                "Timeouts must be 1–180 seconds for export and 1–90 seconds for SSH".into(),
            );
        }
        Ok(())
    }
    fn destination_digest(&self) -> Result<String, String> {
        digest(&(&self.ssh_target, &self.remote_binary, &self.remote_snapshot))
    }
}
fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|e| e.to_string())?)
    ))
}
#[derive(Default, Deserialize, Serialize)]
struct State {
    source_digest: String,
    content_digest: String,
    destination_digest: String,
    last_success: Option<DateTime<Utc>>,
}
#[derive(Serialize)]
struct Status<'a> {
    checked_at: DateTime<Utc>,
    outcome: &'a str,
    detail: Option<&'a str>,
    last_success: Option<DateTime<Utc>>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UnchangedSource,
    UnchangedUsage,
    Transferred,
    WouldTransfer,
    AlreadyRunning,
}
impl Outcome {
    pub fn name(&self) -> &'static str {
        match self {
            Self::UnchangedSource => "unchanged source; no export or SSH",
            Self::UnchangedUsage => "unchanged usage; no SSH",
            Self::Transferred => "transferred",
            Self::WouldTransfer => "dry run: would transfer; SSH not called",
            Self::AlreadyRunning => "another sync is running; skipped",
        }
    }
}

pub trait Operations {
    fn export(&mut self, config: &Config) -> Result<Bundle, String>;
    fn transfer(&mut self, config: &Config, bundle: &Bundle) -> Result<(), String>;
}
pub fn run(config: &Config, dry_run: bool) -> Result<Outcome, String> {
    run_with(config, dry_run, &mut SystemOperations)
}
pub fn run_with(
    config: &Config,
    dry_run: bool,
    ops: &mut impl Operations,
) -> Result<Outcome, String> {
    config.validate()?;
    snapshot::private_dir(&config.state_dir)?;
    let Some(_lock) = Lock::acquire(&config.state_dir.join("sync.lock"))? else {
        return Ok(Outcome::AlreadyRunning);
    };
    clean_temporaries(&config.state_dir)?;
    let state_path = config.state_dir.join("success.json");
    let mut state: State = if state_path.exists() {
        serde_json::from_reader(snapshot::private_file(&state_path)?.take(64 * 1024))
            .map_err(|_| "Sync state is invalid; move success.json aside to retry safely")?
    } else {
        State::default()
    };
    let result: Result<Outcome, String> = (|| {
        let source = manifest(&config.codex_home)?;
        let destination = config.destination_digest()?;
        if source == state.source_digest && destination == state.destination_digest {
            return Ok(Outcome::UnchangedSource);
        }
        let mut bundle = ops.export(config)?;
        if bundle.demo || bundle.records.is_empty() || bundle.coverage.unreadable > 0 {
            return Err("Refusing to sync demo, empty, or unreadable-source data; previous Pi snapshot is preserved".into());
        }
        let stable = manifest(&config.codex_home)? == source;
        // Active Codex sessions must not starve sync. Send a validated prefix but
        // never cache it as the complete current source; the next run rescans.
        if !stable {
            bundle
                .coverage
                .notes
                .push("Local logs changed during this scan; the next sync will rescan.".into());
        }
        let cached_source = if stable { source } else { String::new() };
        bundle.source = "Synced local Codex snapshot".into();
        let content = snapshot::fingerprint(&bundle)?;
        if content == state.content_digest && destination == state.destination_digest {
            if !dry_run {
                state.source_digest = cached_source;
                save_state(&state_path, &state)?;
            }
            return Ok(Outcome::UnchangedUsage);
        }
        if dry_run {
            return Ok(Outcome::WouldTransfer);
        }
        ops.transfer(config, &bundle)?;
        state = State {
            source_digest: cached_source,
            content_digest: content,
            destination_digest: destination,
            last_success: Some(Utc::now()),
        };
        save_state(&state_path, &state)?;
        Ok(Outcome::Transferred)
    })();
    let (outcome, detail) = match &result {
        Ok(o) => (o.name(), None),
        Err(e) => ("failed; will retry next interval", Some(e.as_str())),
    };
    let status = Status {
        checked_at: Utc::now(),
        outcome,
        detail,
        last_success: state.last_success,
    };
    snapshot::atomic_write(
        &config.state_dir.join("status.json"),
        &serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?,
        0o600,
    )?;
    result
}
fn save_state(path: &Path, state: &State) -> Result<(), String> {
    snapshot::atomic_write(
        path,
        &serde_json::to_vec(state).map_err(|e| e.to_string())?,
        0o600,
    )
}

/// Metadata-only fast path: no prompt/token log bytes are read on idle intervals.
pub fn manifest(root: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    let mut bytes = 0;
    let mut roots = 0;
    let mut visited = 0;
    for name in ["sessions", "archived_sessions"] {
        let p = root.join(name);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                roots += 1;
                walk(&p, &mut entries, &mut bytes, &mut visited, 0)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("Codex source directory is unreadable or symlinked".into()),
        }
    }
    if roots == 0 || entries.is_empty() {
        return Err("No local Codex JSONL source files; nothing will be sent".into());
    }
    entries.sort();
    digest(&entries)
}
type Entry = (PathBuf, u64, i64, i64, u64, u64, i64, i64);
fn walk(
    path: &Path,
    entries: &mut Vec<Entry>,
    bytes: &mut u64,
    visited: &mut usize,
    depth: u8,
) -> Result<(), String> {
    if depth > 12 {
        return Err("Source exceeds 12 directory levels".into());
    }
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        *visited += 1;
        if *visited > 20_000 {
            return Err("Source exceeds 20,000 directory entries".into());
        }
        let m = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() {
            return Err("Symlinked source entry; sync will not follow it".into());
        }
        if m.is_dir() {
            walk(&p, entries, bytes, visited, depth + 1)?;
        } else if m.is_file() && p.extension().is_some_and(|e| e == "jsonl") {
            *bytes = bytes.checked_add(m.len()).ok_or("Source size overflow")?;
            if entries.len() >= 5000 || *bytes > 4 * 1024 * 1024 * 1024 {
                return Err(
                    "Source exceeds sync budget (5000 logs or 4 GiB); select a smaller source"
                        .into(),
                );
            }
            entries.push((
                p,
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ino(),
                m.dev(),
                m.ctime(),
                m.ctime_nsec(),
            ));
        }
    }
    Ok(())
}

fn clean_temporaries(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid = name
            .strip_prefix("export-")
            .or_else(|| name.strip_prefix("transfer-"))
            .and_then(|s| s.strip_suffix(".json"));
        if pid.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
            let m = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if !m.is_file() || m.file_type().is_symlink() || m.uid() != unsafe { libc::geteuid() } {
                return Err("Unsafe stale sync temporary file".into());
            }
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct SystemOperations;
impl Operations for SystemOperations {
    fn export(&mut self, c: &Config) -> Result<Bundle, String> {
        let path = c
            .state_dir
            .join(format!("export-{}.json", std::process::id()));
        // A prior crash can leave this private temporary file; a new run owns the lock.
        if fs::symlink_metadata(&path).is_ok() {
            return Err("Stale export temporary file; remove it before retrying".into());
        }
        let _temp = Temporary(path.clone());
        let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .args(["export", "--codex-home"])
            .arg(&c.codex_home)
            .arg("--output")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        bounded(
            &mut command,
            Duration::from_secs(c.export_timeout_seconds),
            "export",
        )?;
        snapshot::read_bundle(snapshot::private_file(&path)?)
    }
    fn transfer(&mut self, c: &Config, bundle: &Bundle) -> Result<(), String> {
        let bytes = serde_json::to_vec(bundle).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > snapshot::MAX_BYTES {
            return Err("Export exceeds 64 MiB transfer limit".into());
        }
        let path = c
            .state_dir
            .join(format!("transfer-{}.json", std::process::id()));
        let _temp = Temporary(path.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        drop(file);
        let remote = format!(
            "{} receive --output {}",
            quote(&c.remote_binary.to_string_lossy()),
            quote(&c.remote_snapshot.to_string_lossy())
        );
        let mut command = Command::new("/usr/bin/ssh");
        command
            .args(ssh_options())
            .arg(&c.ssh_target)
            .arg(remote)
            .stdin(Stdio::from(File::open(&path).map_err(|e| e.to_string())?))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        bounded(
            &mut command,
            Duration::from_secs(c.ssh_timeout_seconds),
            "SSH transfer (existing trusted noninteractive SSH access required)",
        )
    }
}
pub fn ssh_options() -> [&'static str; 26] {
    [
        "-T",
        "-x",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "UpdateHostKeys=no",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ControlPath=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPersist=no",
    ]
}
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
pub fn bounded(command: &mut Command, timeout: Duration, label: &str) -> Result<(), String> {
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start {label}: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "{label} failed ({status}); no successful-sync marker recorded"
                    ))
                };
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            Ok(None) => {}
        }
        if start.elapsed() >= timeout {
            // Kill the dedicated process group, including SSH proxy helpers if any.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label} timed out; retry next interval"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
