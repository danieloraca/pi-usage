//! Validated whole-snapshot replacement, shared by SSH receiving and HTTP reload.
use crate::{Bundle, ingest};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            if !m.is_dir()
                || m.file_type().is_symlink()
                || m.uid() != unsafe { libc::geteuid() }
                || m.mode() & 0o077 != 0
            {
                return Err(
                    "State directory must be owned by you, mode 0700, and not a symlink".into(),
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Do not change permissions on an existing parent directory.
            let mut builder = fs::DirBuilder::new();
            use std::os::unix::fs::DirBuilderExt;
            builder
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|e| e.to_string())?;
            return private_dir(path);
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

pub fn private_file(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return Err("Private file must be owned by you and have mode 0600".into());
    }
    Ok(file)
}

/// Persistent lock file: never unlink it, so simultaneous openers use one inode.
pub struct Lock(File);
impl Lock {
    pub fn acquire(path: &Path) -> Result<Option<Self>, String> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        let m = f.metadata().map_err(|e| e.to_string())?;
        if !m.is_file() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err("Unsafe lock-file ownership or permissions".into());
        }
        // flock is released by the kernel when the process exits, including crashes.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(Self(f)));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e.to_string())
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if let Ok(m) = fs::symlink_metadata(path)
        && (!m.is_file() || m.file_type().is_symlink() || m.uid() != unsafe { libc::geteuid() })
    {
        return Err("Refusing to replace an unsafe destination".into());
    }
    let parent = path
        .parent()
        .ok_or("Destination needs a parent directory")?;
    let name = path
        .file_name()
        .ok_or("Destination needs a filename")?
        .to_string_lossy();
    let temp = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn fingerprint(bundle: &Bundle) -> Result<String, String> {
    // Capture usage and coverage changes, not export time or the source machine path.
    let bytes = serde_json::to_vec(&(
        bundle.schema_version,
        bundle.demo,
        &bundle.records,
        &bundle.coverage,
    ))
    .map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn read_bundle(reader: impl Read) -> Result<Bundle, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Snapshot exceeds 64 MiB".into());
    }
    let bundle: Bundle =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid snapshot: {e}"))?;
    let conflicts = bundle.coverage.conflicts;
    let validated = ingest::validate_import(bundle)?;
    if validated.coverage.conflicts != conflicts {
        return Err("Snapshot contains conflicting record IDs".into());
    }
    Ok(validated)
}

pub fn receive(path: &Path, reader: impl Read) -> Result<&'static str, String> {
    if !path.is_absolute() {
        return Err("Receive destination must be absolute".into());
    }
    let mut bundle = read_bundle(reader)?;
    let _lock = Lock::acquire(&path.with_extension("json.lock"))?
        .ok_or("Another snapshot receiver is running; retry later")?;
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_file() || m.file_type().is_symlink() {
            return Err("Snapshot destination must be a regular file".into());
        }
        if let Ok(current) = read_bundle(File::open(path).map_err(|e| e.to_string())?) {
            if bundle.generated_at < current.generated_at {
                return Err("Refusing an older snapshot".into());
            }
            if fingerprint(&current)? == fingerprint(&bundle)? {
                return Ok("unchanged");
            }
        }
    }
    // Re-serialize only supported usage fields; unknown input fields never persist.
    bundle.source = "Synced local Codex snapshot".into();
    let bytes = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
    atomic_write(path, &bytes, 0o600)?;
    Ok("updated")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: std::time::SystemTime,
    inode: u64,
    device: u64,
}
pub fn stamp(path: &Path) -> Result<Stamp, String> {
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err("Import must be a regular file, not a symlink".into());
    }
    if m.len() > MAX_BYTES {
        return Err("Snapshot exceeds 64 MiB".into());
    }
    Ok(Stamp {
        len: m.len(),
        modified: m.modified().map_err(|e| e.to_string())?,
        inode: m.ino(),
        device: m.dev(),
    })
}

#[derive(Clone, Serialize)]
pub struct ReloadStatus {
    pub last_checked: DateTime<Utc>,
    pub last_loaded: DateTime<Utc>,
    pub reloads: u64,
    pub error: Option<String>,
}
pub struct ImportWatcher {
    path: PathBuf,
    accepted: Stamp,
    pub status: ReloadStatus,
}
impl ImportWatcher {
    pub fn open(path: PathBuf) -> Result<(Self, Bundle), String> {
        let before = stamp(&path)?;
        let b = read_bundle(File::open(&path).map_err(|e| e.to_string())?)?;
        if stamp(&path)? != before {
            return Err("Import changed while reading; retry startup".into());
        }
        let now = Utc::now();
        Ok((
            Self {
                path,
                accepted: before,
                status: ReloadStatus {
                    last_checked: now,
                    last_loaded: now,
                    reloads: 0,
                    error: None,
                },
            },
            b,
        ))
    }
    pub fn poll(&mut self, current: &mut Bundle) {
        if let Some(next) = self.check(current.generated_at) {
            *current = next;
        }
    }
    pub fn check(&mut self, current_time: DateTime<Utc>) -> Option<Bundle> {
        self.status.last_checked = Utc::now();
        let result: Result<Option<Bundle>, String> = (|| {
            let before = stamp(&self.path)?;
            if before == self.accepted {
                return Ok(None);
            }
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.path)
                .map_err(|e| e.to_string())?;
            let next = read_bundle(file)?;
            if stamp(&self.path)? != before {
                return Err("Import changed while reading; keeping last valid snapshot".into());
            }
            if next.generated_at < current_time {
                return Err("Older snapshot rejected; keeping last valid snapshot".into());
            }
            self.accepted = before;
            self.status.last_loaded = Utc::now();
            self.status.reloads += 1;
            Ok(Some(next))
        })();
        match result {
            Ok(next) => {
                self.status.error = None;
                next
            }
            Err(error) => {
                self.status.error = Some(error);
                None
            }
        }
    }
}
