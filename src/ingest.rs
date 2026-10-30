use crate::{Bundle, Counts, Coverage, Record};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

// Bound individual log records; prompt/tool bodies are ignored, never exported.
const MAX_LINE: u64 = 8 * 1024 * 1024;
const MAX_IMPORT: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    timestamp: Option<DateTime<Utc>>,
    #[serde(borrow)]
    payload: &'a RawValue,
}
#[derive(Default, Deserialize)]
struct Session {
    #[serde(default)]
    id: String,
    cwd: Option<String>,
    model_provider: Option<String>,
    forked_from_id: Option<String>,
    timestamp: Option<DateTime<Utc>>,
}
#[derive(Default, Deserialize)]
struct Context {
    cwd: Option<String>,
}
#[derive(Deserialize)]
struct Modern {
    response_id: String,
    thread_id: String,
    usage: Counts,
}
#[derive(Deserialize)]
struct Legacy {
    #[serde(rename = "type")]
    kind: String,
    info: Option<Info>,
}
#[derive(Deserialize)]
struct Info {
    total_token_usage: Counts,
    last_token_usage: Option<Counts>,
}
struct Candidate {
    record: Record,
    cumulative: Option<Counts>,
    last: Option<Counts>,
}

pub fn scan(root: &Path) -> Bundle {
    let mut bundle = Bundle::empty(format!("Local Codex · {}", root.display()));
    let mut candidates = Vec::new();
    let mut found = false;
    for name in ["sessions", "archived_sessions"] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                found = true;
                visit(&path, &mut candidates, &mut bundle.coverage, 0);
            }
            Ok(_) => bundle
                .coverage
                .notes
                .push(format!("{name} is not a regular directory; skipped.")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                bundle.coverage.unreadable += 1;
            }
        }
    }
    if !found {
        bundle.coverage.notes.push("No readable sessions or archived_sessions directory. Select a Codex home or import an export.".into());
    }
    bundle.records = normalize(candidates, &mut bundle.coverage);
    bundle
}

fn visit(path: &Path, candidates: &mut Vec<Candidate>, coverage: &mut Coverage, depth: usize) {
    if depth > 12 {
        coverage.unreadable += 1;
        return;
    }
    let entries = match fs::read_dir(path) {
        Ok(v) => v,
        Err(_) => {
            coverage.unreadable += 1;
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => paths.push(e.path()),
            Err(_) => coverage.unreadable += 1,
        }
    }
    paths.sort();
    for p in paths {
        let meta = match fs::symlink_metadata(&p) {
            Ok(v) => v,
            Err(_) => {
                coverage.unreadable += 1;
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            coverage
                .notes
                .push("Symlinked logs/directories skipped.".into());
            continue;
        }
        if meta.is_dir() {
            visit(&p, candidates, coverage, depth + 1);
        } else if meta.is_file() && p.extension().is_some_and(|v| v == "jsonl") {
            match File::open(&p) {
                Ok(f) => {
                    coverage.files_read += 1;
                    read_log(BufReader::new(f), candidates, coverage);
                }
                Err(_) => coverage.unreadable += 1,
            }
        }
    }
    coverage.notes.sort();
    coverage.notes.dedup();
}

fn read_log(mut reader: impl BufRead, candidates: &mut Vec<Candidate>, coverage: &mut Coverage) {
    let mut session = Session::default();
    let mut context = Context::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => break,
            Err(_) => {
                coverage.unreadable += 1;
                break;
            }
            _ => {}
        }
        if line.len() as u64 > MAX_LINE {
            coverage.malformed += 1;
            if line.last() != Some(&b'\n') && reader.skip_until(b'\n').is_err() {
                coverage.unreadable += 1;
                break;
            }
            continue;
        }
        if line.last() != Some(&b'\n') {
            coverage.incomplete_lines += 1;
            break;
        }
        let event: Envelope = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                coverage.malformed += 1;
                continue;
            }
        };
        match event.kind {
            "session_meta" => match serde_json::from_str(event.payload.get()) {
                Ok(v) => {
                    session = v;
                    context = Context::default();
                }
                Err(_) => {
                    session = Session::default();
                    coverage.malformed += 1;
                }
            },
            "turn_context" => match serde_json::from_str(event.payload.get()) {
                Ok(v) => context = v,
                Err(_) => {
                    context = Context::default();
                    coverage.malformed += 1;
                }
            },
            "token_usage_record" | "event_msg" => {
                let (id, session_id, counts, cumulative, last) =
                    if event.kind == "token_usage_record" {
                        let data: Modern = match serde_json::from_str(event.payload.get()) {
                            Ok(v) => v,
                            Err(_) => {
                                coverage.skipped_usage += 1;
                                continue;
                            }
                        };
                        if data.response_id.is_empty() || data.thread_id.is_empty() {
                            coverage.skipped_usage += 1;
                            continue;
                        }
                        (
                            format!("response:{}", data.response_id),
                            data.thread_id,
                            data.usage,
                            None,
                            None,
                        )
                    } else {
                        let data: Legacy = match serde_json::from_str(event.payload.get()) {
                            Ok(v) => v,
                            Err(_) => {
                                coverage.malformed += 1;
                                continue;
                            }
                        };
                        if data.kind != "token_count" {
                            continue;
                        }
                        let Some(info) = data.info else {
                            coverage.skipped_usage += 1;
                            continue;
                        };
                        if session.id.is_empty() {
                            coverage.skipped_usage += 1;
                            continue;
                        }
                        (
                            String::new(),
                            session.id.clone(),
                            info.total_token_usage,
                            Some(info.total_token_usage),
                            info.last_token_usage,
                        )
                    };
                let Some(timestamp) = event.timestamp else {
                    coverage.skipped_usage += 1;
                    continue;
                };
                if !counts.valid() || last.is_some_and(|c| !c.valid()) {
                    coverage.skipped_usage += 1;
                    continue;
                }
                if session
                    .model_provider
                    .as_deref()
                    .is_some_and(|v| v != "openai")
                {
                    coverage.skipped_usage += 1;
                    continue;
                }
                if session.forked_from_id.is_some() {
                    let Some(created) = session.timestamp else {
                        coverage.skipped_usage += 1;
                        continue;
                    };
                    if timestamp < created {
                        coverage.excluded_history += 1;
                        continue;
                    }
                }
                let project = context
                    .cwd
                    .as_ref()
                    .or(session.cwd.as_ref())
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "Unattributed".into());
                candidates.push(Candidate {
                    record: Record {
                        id,
                        session: session_id,
                        timestamp,
                        project,
                        counts,
                    },
                    cumulative,
                    last,
                });
            }
            _ => {}
        }
    }
}

fn normalize(mut candidates: Vec<Candidate>, coverage: &mut Coverage) -> Vec<Record> {
    let modern_sessions: HashSet<_> = candidates
        .iter()
        .filter(|c| c.cumulative.is_none())
        .map(|c| c.record.session.clone())
        .collect();
    candidates.sort_by(|a, b| {
        (
            &a.record.session,
            a.record.timestamp,
            a.record.counts.total(),
        )
            .cmp(&(
                &b.record.session,
                b.record.timestamp,
                b.record.counts.total(),
            ))
    });
    let mut previous: HashMap<String, Counts> = HashMap::new();
    let mut seen_legacy = HashSet::new();
    let mut records = Vec::new();
    for mut c in candidates {
        if let Some(total) = c.cumulative {
            // Prefer response IDs for a session when available, avoiding mixed-format double counts.
            if modern_sessions.contains(&c.record.session) {
                coverage.legacy_suppressed += 1;
                continue;
            }
            let key = format!(
                "legacy:{}:{}:{}:{}:{:?}",
                c.record.session,
                c.record.timestamp,
                total.input_tokens,
                total.output_tokens,
                total.cached_input_tokens
            );
            if !seen_legacy.insert(key.clone()) {
                coverage.duplicates += 1;
                continue;
            }
            let before = previous.insert(c.record.session.clone(), total);
            let counts = match before {
                Some(p) => match total.difference(p) {
                    Some(delta) => Some(delta),
                    None => {
                        coverage.counter_resets += 1;
                        c.last
                    }
                },
                None => {
                    if c.last != Some(total) {
                        coverage.unknown_baselines += 1;
                    }
                    c.last
                }
            };
            let Some(counts) = counts else {
                coverage.skipped_usage += 1;
                continue;
            };
            if counts.total() == 0 {
                coverage.duplicates += 1;
                continue;
            }
            if !counts.valid() {
                coverage.skipped_usage += 1;
                continue;
            }
            c.record.id = key;
            c.record.counts = counts;
        }
        records.push(c.record);
    }
    deduplicate(records, coverage)
}

fn deduplicate(records: Vec<Record>, coverage: &mut Coverage) -> Vec<Record> {
    let mut unique: BTreeMap<String, Record> = BTreeMap::new();
    let mut conflicts = HashSet::new();
    for record in records {
        if conflicts.contains(&record.id) {
            coverage.duplicates += 1;
            continue;
        }
        if let Some(old) = unique.get(&record.id) {
            if old.counts == record.counts
                && old.timestamp == record.timestamp
                && old.session == record.session
                && old.project == record.project
            {
                coverage.duplicates += 1;
            } else {
                // Ambiguous copies are excluded, never arbitrarily chosen.
                coverage.conflicts += 1;
                conflicts.insert(record.id.clone());
                unique.remove(&record.id);
            }
        } else {
            unique.insert(record.id.clone(), record);
        }
    }
    let mut out: Vec<_> = unique.into_values().collect();
    out.sort_by_key(|r| r.timestamp);
    out
}

pub fn import(path: &Path) -> Result<Bundle, String> {
    let file = File::open(path).map_err(|e| format!("Cannot read import: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_IMPORT {
        return Err("Import exceeds 64 MiB".into());
    }
    let bundle: Bundle = serde_json::from_reader(file.take(MAX_IMPORT + 1))
        .map_err(|e| format!("Invalid export: {e}"))?;
    validate_import(bundle)
}

pub fn validate_import(mut bundle: Bundle) -> Result<Bundle, String> {
    if bundle.schema_version != 1 {
        return Err("Unsupported export schema; expected version 1".into());
    }
    for r in &bundle.records {
        if r.id.is_empty() || r.session.is_empty() || r.project.is_empty() || !r.counts.valid() {
            return Err("Export contains invalid usage records".into());
        }
    }
    bundle.records = deduplicate(bundle.records, &mut bundle.coverage);
    bundle.source = "Imported Codex snapshot".into();
    Ok(bundle)
}

pub fn demo() -> Bundle {
    let mut bundle = Bundle::empty("Built-in sample data".into());
    bundle.demo = true;
    // Exercise the same parser as real data. Shift fictional dates to the current UTC week.
    let mut candidates = Vec::new();
    read_log(
        BufReader::new(include_bytes!("../fixtures/demo.jsonl").as_slice()),
        &mut candidates,
        &mut bundle.coverage,
    );
    bundle.records = normalize(candidates, &mut bundle.coverage);
    let latest = bundle
        .records
        .iter()
        .map(|r| r.timestamp.date_naive())
        .max()
        .unwrap();
    let shift = Utc::now().date_naive() - latest;
    for r in &mut bundle.records {
        r.timestamp += shift;
    }
    bundle.coverage.files_read = 1;
    bundle.coverage.notes.push(
        "Fictional demo records; dates shifted to this week. Never combined with real usage."
            .into(),
    );
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn parse(lines: Vec<serde_json::Value>) -> Bundle {
        let text = lines
            .into_iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>();
        let mut b = Bundle::empty("test".into());
        let mut candidates = Vec::new();
        read_log(
            BufReader::new(text.as_bytes()),
            &mut candidates,
            &mut b.coverage,
        );
        b.records = normalize(candidates, &mut b.coverage);
        b
    }
    fn meta() -> serde_json::Value {
        json!({"type":"session_meta","payload":{"id":"s1","cwd":"/work/pi","model_provider":"openai"}})
    }
    fn legacy(at: &str, total: u64, last: Option<u64>) -> serde_json::Value {
        let counts = |n| json!({"input_tokens":n,"output_tokens":n/10,"cached_input_tokens":0});
        json!({"type":"event_msg","timestamp":at,"payload":{"type":"token_count","info":{"total_token_usage":counts(total),"last_token_usage":last.map(counts)}}})
    }
    fn modern(id: &str, n: u64) -> serde_json::Value {
        json!({"type":"token_usage_record","timestamp":"2026-10-29T12:00:00Z","payload":{"thread_id":"s1","response_id":id,"usage":{"input_tokens":n,"output_tokens":10}}})
    }
    #[test]
    fn dedups_cumulative_copies_and_repeated_notifications() {
        let a = legacy("2026-10-28T10:00:00Z", 100, Some(100));
        let b = legacy("2026-10-29T10:00:00Z", 300, Some(200));
        let out = parse(vec![
            meta(),
            a.clone(),
            b.clone(),
            meta(),
            a,
            b,
            legacy("2026-10-29T10:01:00Z", 300, Some(200)),
        ]);
        assert_eq!(out.records.len(), 2);
        assert_eq!(
            out.records.iter().map(|r| r.counts.total()).sum::<u64>(),
            330
        );
        assert_eq!(out.coverage.duplicates, 3);
    }
    #[test]
    fn no_first_cumulative_guess_and_reset_uses_last() {
        let out = parse(vec![
            meta(),
            legacy("2026-10-28T10:00:00Z", 900, None),
            legacy("2026-10-29T10:00:00Z", 100, Some(30)),
        ]);
        assert_eq!(out.records[0].counts.total(), 33);
        assert_eq!(out.coverage.unknown_baselines, 1);
        assert_eq!(out.coverage.counter_resets, 1);
    }
    #[test]
    fn modern_wins_even_when_legacy_comes_first() {
        let out = parse(vec![
            meta(),
            legacy("2026-10-28T10:00:00Z", 100, Some(100)),
            modern("r1", 100),
            modern("r1", 100),
        ]);
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.coverage.legacy_suppressed, 1);
        assert_eq!(out.coverage.duplicates, 1);
    }
    #[test]
    fn conflicting_response_ids_are_excluded() {
        let out = parse(vec![meta(), modern("r1", 100), modern("r1", 200)]);
        assert!(out.records.is_empty());
        assert_eq!(out.coverage.conflicts, 1);
    }
    #[test]
    fn missing_and_negative_usage_are_not_zero() {
        let mut bad = modern("r1", 100);
        bad["payload"]["usage"]["input_tokens"] = json!(-1);
        let out = parse(vec![
            meta(),
            bad,
            json!({"type":"event_msg","timestamp":"2026-10-29T00:00:00Z","payload":{"type":"token_count","info":null}}),
        ]);
        assert!(out.records.is_empty());
        assert_eq!(out.coverage.skipped_usage, 2);
    }
    #[test]
    fn truncated_and_malformed_lines_are_reported() {
        let mut c = Coverage::default();
        let mut records = Vec::new();
        read_log(
            BufReader::new(b"not-json\n{\"unfinished\":".as_slice()),
            &mut records,
            &mut c,
        );
        assert_eq!(c.malformed, 1);
        assert_eq!(c.incomplete_lines, 1);
    }
    #[test]
    fn inherited_fork_history_is_excluded() {
        let mut m = meta();
        m["payload"]["forked_from_id"] = json!("parent");
        m["payload"]["timestamp"] = json!("2026-10-29T00:00:00Z");
        let out = parse(vec![
            m,
            legacy("2026-10-28T10:00:00Z", 100, Some(100)),
            legacy("2026-10-29T10:00:00Z", 200, Some(100)),
        ]);
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.records[0].counts.total(), 110);
        assert_eq!(out.coverage.excluded_history, 1);
    }
    #[test]
    fn missing_root_is_unavailable() {
        let out = scan(Path::new("/path/that/does/not/exist/pi-usage-test"));
        assert!(out.records.is_empty());
        assert!(!out.coverage.notes.is_empty());
    }
    #[test]
    fn imports_dedup_and_preserve_demo_provenance() {
        let mut b = demo();
        b.records.push(b.records[0].clone());
        let count = b.records.len() - 1;
        let out = validate_import(b).unwrap();
        assert!(out.demo);
        assert_eq!(out.records.len(), count);
        assert!(out.coverage.duplicates > 0);
        let mut b = demo();
        b.schema_version = 2;
        assert!(validate_import(b).is_err());
    }
}
