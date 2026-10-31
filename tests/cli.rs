use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pi-usage-test-{}-{stamp}-{seq}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pi-usage"))
}
fn request(addr: &str, path: &str, method: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}
#[test]
fn export_roundtrip_no_overwrite_and_no_prompt_content() {
    let temp = Temp::new();
    let path = temp.0.join("export.json");
    let out = bin()
        .args(["export", "--demo", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = fs::read_to_string(&path).unwrap();
    let value: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(value["demo"], true);
    assert_eq!(value["records"].as_array().unwrap().len(), 7);
    assert!(!content.contains("payload"));
    assert!(!content.contains("prompt"));
    let fail = bin()
        .args(["export", "--demo", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!fail.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
    assert!(pi_usage::ingest::import(&path).unwrap().demo);
}
#[test]
fn copied_log_files_deduplicate_without_mutation() {
    let temp = Temp::new();
    fs::create_dir(temp.0.join("sessions")).unwrap();
    fs::create_dir(temp.0.join("archived_sessions")).unwrap();
    let fixture = include_bytes!("../fixtures/demo.jsonl");
    let path = temp.0.join("sessions/one.jsonl");
    fs::write(&path, fixture).unwrap();
    fs::write(temp.0.join("archived_sessions/copy.jsonl"), fixture).unwrap();
    let b = pi_usage::ingest::scan(&temp.0);
    assert_eq!(b.records.len(), 7);
    assert_eq!(b.coverage.duplicates, 7);
    assert_eq!(b.coverage.files_read, 2);
    assert_eq!(fs::read(&path).unwrap(), fixture);
}
#[test]
fn http_serves_ui_api_and_unavailable_state() {
    let temp = Temp::new();
    let child = bin()
        .args(["serve", "--codex-home"])
        .arg(&temp.0)
        .args(["--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = Running(child);
    let mut line = String::new();
    BufReader::new(running.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .split("http://")
        .nth(1)
        .expect("server must print its URL")
        .split_whitespace()
        .next()
        .unwrap();
    let ui = request(addr, "/", "GET");
    assert!(ui.starts_with("HTTP/1.1 200"));
    assert!(ui.contains("Token Grove"));
    assert!(ui.to_lowercase().contains("content-security-policy"));
    let response = request(addr, "/api/usage?days=30", "GET");
    let v: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(v["status"], "unavailable");
    assert!(v["totals"].is_null());
    assert_eq!(v["cloud"], "unmeasured");
    assert_eq!(v["demo"], false);
    assert!(request(addr, "/api/usage?days=invalid", "GET").starts_with("HTTP/1.1 400"));
    assert!(request(addr, "/missing", "GET").starts_with("HTTP/1.1 404"));
    assert!(request(addr, "/api/usage", "POST").starts_with("HTTP/1.1 405"));
}

fn api(addr: &str) -> Value {
    let response = request(addr, "/api/usage?days=all", "GET");
    serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}
fn wait_for_api(addr: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let value = api(addr);
        if predicate(&value) {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Reload did not become observable: {value}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[test]
fn imported_http_snapshot_reloads_without_restart_and_survives_bad_update() {
    let temp = Temp::new();
    let path = temp.0.join("snapshot.json");
    let mut bundle = pi_usage::ingest::demo();
    let bytes = serde_json::to_vec(&bundle).unwrap();
    pi_usage::snapshot::receive(&path, std::io::Cursor::new(bytes)).unwrap();
    let child = bin()
        .arg("serve")
        .arg("--import")
        .arg(&path)
        .args(["--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = Running(child);
    let mut line = String::new();
    BufReader::new(running.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .split("http://")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let before = api(addr);
    pi_usage::snapshot::atomic_write(&path, b"partial snapshot", 0o600).unwrap();
    let stale = wait_for_api(addr, |v| v["reload"]["error"].is_string());
    assert_eq!(stale["totals"], before["totals"]);
    bundle.generated_at += chrono::Duration::seconds(1);
    bundle.records[0].counts.input_tokens += 100;
    pi_usage::snapshot::receive(
        &path,
        std::io::Cursor::new(serde_json::to_vec(&bundle).unwrap()),
    )
    .unwrap();
    let updated = wait_for_api(addr, |v| v["reload"]["reloads"] == 1);
    assert!(updated["reload"]["error"].is_null());
    assert_eq!(
        updated["totals"]["total"].as_u64().unwrap(),
        before["totals"]["total"].as_u64().unwrap() + 100
    );
    assert_eq!(updated["totals"]["records"], before["totals"]["records"]);
    assert_eq!(updated["cloud"], "unmeasured");
}

#[test]
fn real_exporter_dry_run_has_no_transport_or_persistent_success() {
    let temp = Temp::new();
    let root = temp.0.join("codex");
    fs::create_dir_all(root.join("sessions")).unwrap();
    let mut fixture = include_str!("../fixtures/demo.jsonl").to_string();
    fixture.push_str("{\"type\":\"response_item\",\"payload\":{\"private_text\":\"DO NOT EXPORT THIS MESSAGE\"}}\n");
    fs::write(root.join("sessions/test.jsonl"), fixture).unwrap();
    let state = temp.0.join("state");
    let config = temp.0.join("sync.json");
    let value = serde_json::json!({"codex_home":root,"state_dir":state,"ssh_target":"unused@test.invalid","remote_binary":"/unused/pi-usage","remote_snapshot":"/unused/usage.json","export_timeout_seconds":5,"ssh_timeout_seconds":1});
    pi_usage::snapshot::atomic_write(&config, &serde_json::to_vec(&value).unwrap(), 0o600).unwrap();
    let output = bin()
        .args(["sync", "--config"])
        .arg(&config)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("SSH not called"));
    assert!(!state.join("success.json").exists());
    let mut names = fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["status.json", "sync.lock"]);
    let export = temp.0.join("usage.json");
    let output = bin()
        .args(["export", "--codex-home"])
        .arg(root)
        .arg("--output")
        .arg(&export)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !fs::read_to_string(export)
            .unwrap()
            .contains("DO NOT EXPORT")
    );
}
