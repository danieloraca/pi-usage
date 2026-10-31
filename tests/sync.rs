use pi_usage::{
    Bundle, agent, ingest,
    snapshot::{self, ImportWatcher, Lock},
    sync::{self, Config, Operations, Outcome},
};
use std::{
    fs,
    io::Cursor,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "token-grove-sync-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config(t: &Temp) -> Config {
    let root = t.0.join("codex");
    fs::create_dir_all(root.join("sessions")).unwrap();
    fs::write(
        root.join("sessions/example.jsonl"),
        include_bytes!("../fixtures/demo.jsonl"),
    )
    .unwrap();
    Config {
        codex_home: root,
        state_dir: t.0.join("state"),
        ssh_target: "test@pi.example".into(),
        remote_binary: PathBuf::from("/home/test/pi-usage"),
        remote_snapshot: PathBuf::from("/home/test/usage.json"),
        export_timeout_seconds: 2,
        ssh_timeout_seconds: 2,
    }
}
fn sample() -> Bundle {
    let mut b = ingest::demo();
    b.demo = false;
    b
}
struct Fake {
    exports: usize,
    transfers: usize,
    fail: bool,
    bundle: Bundle,
}
impl Fake {
    fn new() -> Self {
        Self {
            exports: 0,
            transfers: 0,
            fail: false,
            bundle: sample(),
        }
    }
}
impl Operations for Fake {
    fn export(&mut self, _: &Config) -> Result<Bundle, String> {
        self.exports += 1;
        let mut b = self.bundle.clone();
        b.generated_at = chrono::Utc::now();
        Ok(b)
    }
    fn transfer(&mut self, _: &Config, _: &Bundle) -> Result<(), String> {
        self.transfers += 1;
        if self.fail {
            Err("offline".into())
        } else {
            Ok(())
        }
    }
}
fn touch(c: &Config) {
    let p = c.codex_home.join("sessions/example.jsonl");
    let mut bytes = fs::read(&p).unwrap();
    bytes.push(b'\n');
    fs::write(p, bytes).unwrap();
}
#[test]
fn unchanged_source_skips_export_and_unchanged_usage_skips_ssh() {
    let t = Temp::new();
    let c = config(&t);
    let mut ops = Fake::new();
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::Transferred
    );
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::UnchangedSource
    );
    assert_eq!((ops.exports, ops.transfers), (1, 1));
    touch(&c);
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::UnchangedUsage
    );
    assert_eq!((ops.exports, ops.transfers), (2, 1));
    touch(&c);
    ops.bundle.records[0].counts.input_tokens += 100;
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::Transferred
    );
    assert_eq!(ops.transfers, 2);
}
#[test]
fn offline_failure_retries_without_advancing_success_and_destination_changes_force_send() {
    let t = Temp::new();
    let mut c = config(&t);
    let mut ops = Fake::new();
    ops.fail = true;
    assert!(sync::run_with(&c, false, &mut ops).is_err());
    assert!(!c.state_dir.join("success.json").exists());
    ops.fail = false;
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::Transferred
    );
    c.ssh_target = "other@pi.example".into();
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::Transferred
    );
    assert_eq!(ops.transfers, 3);
}
#[test]
fn dry_run_never_transfers_or_claims_success() {
    let t = Temp::new();
    let c = config(&t);
    let mut ops = Fake::new();
    assert_eq!(
        sync::run_with(&c, true, &mut ops).unwrap(),
        Outcome::WouldTransfer
    );
    assert_eq!(ops.transfers, 0);
    assert!(!c.state_dir.join("success.json").exists());
}
#[test]
fn empty_demo_or_unreadable_exports_preserve_remote() {
    let t = Temp::new();
    let c = config(&t);
    let mut ops = Fake::new();
    ops.bundle.demo = true;
    assert!(sync::run_with(&c, false, &mut ops).is_err());
    ops.bundle.demo = false;
    ops.bundle.coverage.unreadable = 1;
    assert!(sync::run_with(&c, false, &mut ops).is_err());
    ops.bundle.coverage.unreadable = 0;
    ops.bundle.records.clear();
    assert!(sync::run_with(&c, false, &mut ops).is_err());
    assert_eq!(ops.transfers, 0);
}
#[test]
fn active_logs_do_not_starve_sync_and_force_a_later_rescan() {
    struct Changing {
        exports: usize,
        transfers: usize,
    }
    impl Operations for Changing {
        fn export(&mut self, c: &Config) -> Result<Bundle, String> {
            self.exports += 1;
            touch(c);
            Ok(sample())
        }
        fn transfer(&mut self, _: &Config, b: &Bundle) -> Result<(), String> {
            self.transfers += 1;
            assert!(
                b.coverage
                    .notes
                    .iter()
                    .any(|s| s.contains("next sync will rescan"))
            );
            Ok(())
        }
    }
    let t = Temp::new();
    let c = config(&t);
    let mut ops = Changing {
        exports: 0,
        transfers: 0,
    };
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::Transferred
    );
    assert_eq!(
        sync::run_with(&c, false, &mut ops).unwrap(),
        Outcome::UnchangedUsage
    );
    assert_eq!((ops.exports, ops.transfers), (2, 1));
}
#[test]
fn concurrent_runs_skip_and_kernel_lock_is_reusable() {
    struct Blocking(Arc<Barrier>, Arc<Barrier>);
    impl Operations for Blocking {
        fn export(&mut self, _: &Config) -> Result<Bundle, String> {
            self.0.wait();
            self.1.wait();
            Ok(sample())
        }
        fn transfer(&mut self, _: &Config, _: &Bundle) -> Result<(), String> {
            Ok(())
        }
    }
    let t = Temp::new();
    let c = config(&t);
    let a = Arc::new(Barrier::new(2));
    let b = Arc::new(Barrier::new(2));
    let cc = c.clone();
    let aa = a.clone();
    let bb = b.clone();
    let first = thread::spawn(move || sync::run_with(&cc, false, &mut Blocking(aa, bb)));
    a.wait();
    let mut second = Fake::new();
    assert_eq!(
        sync::run_with(&c, false, &mut second).unwrap(),
        Outcome::AlreadyRunning
    );
    assert_eq!(second.exports, 0);
    b.wait();
    assert_eq!(first.join().unwrap().unwrap(), Outcome::Transferred);
    assert!(
        Lock::acquire(&c.state_dir.join("sync.lock"))
            .unwrap()
            .is_some()
    );
}
#[test]
fn receive_and_reload_preserve_good_snapshot_on_partial_bad_and_older_data() {
    let t = Temp::new();
    let path = t.0.join("usage.json");
    let mut bundle = sample();
    let first = serde_json::to_vec(&bundle).unwrap();
    assert_eq!(
        snapshot::receive(&path, Cursor::new(&first)).unwrap(),
        "updated"
    );
    let (mut watcher, mut current) = ImportWatcher::open(path.clone()).unwrap();
    let n = current.records.len();
    assert!(snapshot::receive(&path, Cursor::new(b"{\"records\":")).is_err());
    assert_eq!(current.records.len(), n);
    let saved = fs::read(&path).unwrap();
    assert_eq!(
        snapshot::receive(&path, Cursor::new(&first)).unwrap(),
        "unchanged"
    );
    assert_eq!(fs::read(&path).unwrap(), saved);
    snapshot::atomic_write(&path, b"not-json", 0o600).unwrap();
    watcher.poll(&mut current);
    assert!(watcher.status.error.is_some());
    assert_eq!(current.records.len(), n);
    bundle.generated_at += chrono::Duration::seconds(1);
    bundle.records[0].counts.input_tokens += 50;
    let second = serde_json::to_vec(&bundle).unwrap();
    snapshot::receive(&path, Cursor::new(&second)).unwrap();
    watcher.poll(&mut current);
    assert!(watcher.status.error.is_none());
    assert_eq!(watcher.status.reloads, 1);
    assert_eq!(current.records.len(), n);
    assert_eq!(current.records[0].counts, bundle.records[0].counts);
    watcher.poll(&mut current);
    assert_eq!(watcher.status.reloads, 1);
    assert_eq!(current.records.len(), n);
    assert!(
        snapshot::receive(&path, Cursor::new(&first))
            .unwrap_err()
            .contains("older")
    );
    snapshot::atomic_write(&path, &first, 0o600).unwrap();
    watcher.poll(&mut current);
    assert!(watcher.status.error.as_ref().unwrap().contains("Older"));
    assert_eq!(current.records[0].counts, bundle.records[0].counts);
}
#[test]
fn receiver_deduplicates_and_strips_unknown_fields_but_rejects_conflicts() {
    let t = Temp::new();
    let path = t.0.join("usage.json");
    let mut b = sample();
    let n = b.records.len();
    b.records.push(b.records[0].clone());
    let mut value = serde_json::to_value(&b).unwrap();
    value["conversation_text"] = serde_json::json!("must never persist");
    snapshot::receive(&path, Cursor::new(serde_json::to_vec(&value).unwrap())).unwrap();
    let saved = fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&saved).contains("must never persist"));
    assert_eq!(
        snapshot::read_bundle(Cursor::new(&saved))
            .unwrap()
            .records
            .len(),
        n
    );
    b.records.last_mut().unwrap().counts.input_tokens += 1;
    assert!(snapshot::receive(&path, Cursor::new(serde_json::to_vec(&b).unwrap())).is_err());
    assert_eq!(fs::read(&path).unwrap(), saved);
}
#[test]
fn permissions_and_symlinks_are_checked() {
    let t = Temp::new();
    let c = config(&t);
    let config_path = t.0.join("config.json");
    fs::write(&config_path, serde_json::to_vec(&c).unwrap()).unwrap();
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Config::read(&config_path).is_err());
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Config::read(&config_path).is_ok());
    let link = t.0.join("config-link");
    symlink(&config_path, &link).unwrap();
    assert!(Config::read(&link).is_err());
    symlink(&config_path, t.0.join("lock")).unwrap();
    assert!(Lock::acquire(&t.0.join("lock")).is_err());
    let path = t.0.join("usage.json");
    snapshot::receive(&path, Cursor::new(serde_json::to_vec(&sample()).unwrap())).unwrap();
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::create_dir(&c.state_dir).unwrap();
    fs::set_permissions(&c.state_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(snapshot::private_dir(&c.state_dir).is_err());
}
#[test]
fn timed_out_subprocess_is_bounded_and_does_not_need_network() {
    let start = Instant::now();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 5"]);
    assert!(
        sync::bounded(&mut command, Duration::from_millis(80), "test child")
            .unwrap_err()
            .contains("timed out")
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn launchagent_escapes_paths_and_has_no_shell_or_credential_setup() {
    let p = agent::plist(
        &PathBuf::from("/Users/test/A&B/sync.json"),
        &PathBuf::from("/Users/test/a b/pi-usage"),
    );
    assert!(p.contains("A&amp;B"));
    assert!(p.contains("<key>StartCalendarInterval</key>"));
    assert_eq!(p.matches("<key>Minute</key>").count(), 12);
    assert!(p.contains("<key>RunAtLoad</key><true/>"));
    assert!(!p.contains("/bin/sh"));
    assert!(!p.contains("KeepAlive"));
    let options = sync::ssh_options();
    assert!(options.contains(&"BatchMode=yes"));
    assert!(options.contains(&"StrictHostKeyChecking=yes"));
    assert!(options.contains(&"UpdateHostKeys=no"));
    assert_eq!(sync::quote("a'b"), "'a'\"'\"'b'");
}
#[test]
fn idle_fast_path_reports_metadata_only_cost() {
    let t = Temp::new();
    let c = config(&t);
    for i in 0..265 {
        fs::write(
            c.codex_home.join(format!("sessions/{i}.jsonl")),
            include_bytes!("../fixtures/demo.jsonl"),
        )
        .unwrap();
    }
    let mut ops = Fake::new();
    sync::run_with(&c, false, &mut ops).unwrap();
    let start = Instant::now();
    for _ in 0..10 {
        assert_eq!(
            sync::run_with(&c, false, &mut ops).unwrap(),
            Outcome::UnchangedSource
        );
    }
    println!(
        "266-file idle fast path: {:?} per run (10 runs), exporter calls={}, fake transport calls={}",
        start.elapsed() / 10,
        ops.exports,
        ops.transfers
    );
    assert_eq!((ops.exports, ops.transfers), (1, 1));
}

#[test]
fn interrupted_transfer_files_are_cleaned_but_other_state_is_preserved() {
    let t = Temp::new();
    let c = config(&t);
    snapshot::private_dir(&c.state_dir).unwrap();
    fs::write(c.state_dir.join("export-999999.json"), b"unfinished").unwrap();
    fs::write(c.state_dir.join("transfer-999999.json"), b"unfinished").unwrap();
    fs::write(c.state_dir.join("user-notes.txt"), b"retain").unwrap();
    sync::run_with(&c, true, &mut Fake::new()).unwrap();
    assert!(!c.state_dir.join("export-999999.json").exists());
    assert!(!c.state_dir.join("transfer-999999.json").exists());
    assert!(c.state_dir.join("user-notes.txt").exists());
}
