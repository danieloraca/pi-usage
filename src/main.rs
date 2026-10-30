use chrono::Utc;
use pi_usage::{Bundle, ingest, report};
use std::{
    env,
    fs::OpenOptions,
    io::{self, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};
use tiny_http::{Header, Method, Response, Server};

const HELP: &str = "Token Grove — local Codex token dashboard\n\nUsage:\n  pi-usage serve [--codex-home DIR | --import FILE | --demo] [--bind IP:PORT] [--refresh SECONDS]\n  pi-usage export [--codex-home DIR | --import FILE | --demo] --output FILE\n  pi-usage --help\n\nDefaults: $CODEX_HOME or ~/.codex; 127.0.0.1:8787; refresh every 60 seconds.\nExport files are created exclusively (existing files are never replaced).\nImported snapshots are fixed at startup. Demo data is always labeled.\n";

#[derive(Clone)]
enum Source {
    Local(PathBuf),
    Import(PathBuf),
    Demo,
}
impl Source {
    fn load(&self) -> Result<Bundle, String> {
        match self {
            Self::Local(p) => Ok(ingest::scan(p)),
            Self::Import(p) => ingest::import(p),
            Self::Demo => Ok(ingest::demo()),
        }
    }
}
struct Options {
    command: String,
    source: Source,
    bind: SocketAddr,
    refresh: u64,
    output: Option<PathBuf>,
}
fn options(args: &[String]) -> Result<Options, String> {
    let command = args.first().map(String::as_str).unwrap_or("serve");
    if !["serve", "export"].contains(&command) {
        return Err(format!("Unknown command: {command}"));
    }
    let root = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .ok_or("Set --codex-home or HOME")?;
    let mut opt = Options {
        command: command.into(),
        source: Source::Local(root),
        bind: "127.0.0.1:8787".parse().unwrap(),
        refresh: 60,
        output: None,
    };
    let mut source_set = false;
    let mut i = 1;
    while i < args.len() {
        let flag = &args[i];
        if ["--demo", "--codex-home", "--import"].contains(&flag.as_str()) {
            if source_set {
                return Err("Choose only one source: --demo, --codex-home, or --import".into());
            }
            source_set = true;
        }
        if flag == "--demo" {
            opt.source = Source::Demo;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("Missing value for {flag}"))?;
        match flag.as_str() {
            "--codex-home" => opt.source = Source::Local(value.into()),
            "--import" => opt.source = Source::Import(value.into()),
            "--output" => opt.output = Some(value.into()),
            "--bind" => {
                opt.bind = value
                    .parse()
                    .map_err(|_| "--bind needs an IP:PORT address")?
            }
            "--refresh" => {
                opt.refresh = value.parse().map_err(|_| "Invalid --refresh seconds")?;
                if opt.refresh < 5 {
                    return Err("--refresh must be at least 5 seconds".into());
                }
            }
            _ => return Err(format!("Unknown option: {flag}")),
        }
        i += 2;
    }
    if command == "export" && opt.output.is_none() {
        return Err("export requires --output FILE".into());
    }
    if command == "serve" && opt.output.is_some() {
        return Err("--output is only valid with export".into());
    }
    Ok(opt)
}

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("pi-usage: {e}");
        std::process::exit(1);
    }
}
fn run(args: &[String]) -> Result<(), String> {
    let opt = options(args)?;
    let bundle = opt.source.load()?;
    if opt.command == "export" {
        let path = opt.output.unwrap();
        let bytes = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
        file.write_all(&bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        println!(
            "Exported {} usage records to {}{}",
            bundle.records.len(),
            path.display(),
            if bundle.demo { " (DEMO)" } else { "" }
        );
        return Ok(());
    }
    let server = Server::http(opt.bind).map_err(|e| e.to_string())?;
    let shared = Arc::new(RwLock::new(bundle));
    if matches!(opt.source, Source::Local(_)) {
        let data = Arc::clone(&shared);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(opt.refresh));
                if let Ok(next) = opt.source.load() {
                    *data.write().unwrap() = next;
                }
            }
        });
    }
    println!(
        "Token Grove: http://{} · Ctrl-C to stop",
        server.server_addr()
    );
    if !opt.bind.ip().is_loopback() {
        eprintln!(
            "Listening beyond loopback. Usage and project paths are visible to clients; this v1 has no authentication."
        );
    }
    io::stdout().flush().map_err(|e| e.to_string())?;
    for request in server.incoming_requests() {
        let (status, mime, body) = if request.method() != &Method::Get {
            (405, "text/plain; charset=utf-8", "GET only".into())
        } else {
            match request.url().split('?').next().unwrap_or("") {
                "/" => (
                    200,
                    "text/html; charset=utf-8",
                    include_str!("../static/index.html").into(),
                ),
                "/app.css" => (
                    200,
                    "text/css; charset=utf-8",
                    include_str!("../static/app.css").into(),
                ),
                "/app.js" => (
                    200,
                    "text/javascript; charset=utf-8",
                    include_str!("../static/app.js").into(),
                ),
                "/health" => (200, "application/json", "{\"status\":\"ok\"}".into()),
                "/api/usage" => match range(request.url()) {
                    Ok(days) => {
                        let data = shared.read().unwrap();
                        (
                            200,
                            "application/json",
                            serde_json::to_string(&report::summarize(&data, days, Utc::now()))
                                .unwrap(),
                        )
                    }
                    Err(e) => (400, "text/plain; charset=utf-8", e.into()),
                },
                _ => (404, "text/plain; charset=utf-8", "Not found".into()),
            }
        };
        let mut response = Response::from_string(body).with_status_code(status);
        for (k, v) in [
            ("Content-Type", mime),
            ("Cache-Control", "no-store"),
            ("X-Content-Type-Options", "nosniff"),
            (
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; connect-src 'self'",
            ),
        ] {
            response.add_header(Header::from_bytes(k, v).unwrap());
        }
        let _ = request.respond(response);
    }
    Ok(())
}
fn range(url: &str) -> Result<Option<u32>, &'static str> {
    match url.split_once('?').map(|(_, q)| q) {
        None | Some("days=30") => Ok(Some(30)),
        Some("days=7") => Ok(Some(7)),
        Some("days=90") => Ok(Some(90)),
        Some("days=all") => Ok(None),
        _ => Err("Use days=7, days=30, days=90, or days=all"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_sources_and_bad_ranges() {
        assert!(options(&["serve", "--demo", "--import", "x"].map(String::from)).is_err());
        assert!(range("/api/usage?days=0").is_err());
        assert_eq!(range("/api/usage?days=all").unwrap(), None);
    }
}
