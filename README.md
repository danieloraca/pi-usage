# Token Grove

A small Rust dashboard for measurable **local Codex token usage**, designed to run on a Raspberry Pi or a desktop. Project directory and executable: `pi-usage`.

Daily UTC totals, project rankings, input/output composition, and explicit data coverage. One binary embeds the HTML, CSS, and JavaScript. No Node build, database, API key, telemetry, cloud service, or voice features.

## Run

Requires an existing Rust 1.88+ toolchain. From this directory:

```sh
cargo run --release --locked -- serve
```

Open **http://127.0.0.1:8787**. The default source is `$CODEX_HOME`, or `~/.codex`. Only `sessions/` and `archived_sessions/` JSONL files are read. Logs are never changed. A missing source opens an honest unavailable dashboard, without sample numbers.

```sh
# A separate, clearly labeled fictional example
cargo run --release --locked -- serve --demo

# A specific local Codex home; optionally choose another port
cargo run --release --locked -- serve --codex-home /path/to/.codex --bind 127.0.0.1:8788

# Show CLI options
cargo run --locked -- --help
```

Live sources are rescanned every 60 seconds (`--refresh SECONDS`, minimum 5). The browser polls the cached report every 60 seconds; Refresh view fetches that report immediately. It does not force a log rescan. Source timestamps are visible. Stop the service with Ctrl-C.

## Portable import workflow

If Codex runs on the computer and the dashboard runs on the Pi, export only normalized usage metadata on the computer:

```sh
cargo run --release --locked -- export --output usage-export-2026-10-30.json
```

Copy that file to the Pi using your existing, authorized transfer method. On the Pi, build from this source with its existing Rust toolchain, then run:

```sh
cargo build --release --locked
./target/release/pi-usage serve --import /path/to/usage-export-2026-10-30.json
```

Exports contain token counts, timestamps, session/response identifiers, project working-directory paths, and coverage diagnostics. They contain **no prompts, responses, tool bodies, or credentials**. Treat project names and paths as private. Export refuses to replace an existing file; use a new filename. Imports accept schema version 1, validate counts, deduplicate identifiers, preserve the demo label and original snapshot timestamp, and reject files over 64 MiB. Imports load once at startup. Restart with a newer export to update the Pi.

The service defaults to loopback. For a trusted LAN only, explicitly use `--bind 0.0.0.0:8787`; this v1 has no authentication or TLS and exposes project paths. No Pi connection, deployment, boot service, software installation, firewall changes, credentials, or publishing is performed by this project. A macOS build cannot be copied to Linux and run; build on the Pi or use a separately configured Linux ARM cross-compilation toolchain. ARM hardware validation is still outstanding.

## What is measured

The local format was checked against the existing `llm-usage` reader and accessible local Codex logs. This is a versioned local-log adapter, not an account billing API.

- `token_usage_record`: per-response `input_tokens`, `output_tokens`, optional `cached_input_tokens`, `response_id`, `thread_id`, and an RFC3339 timestamp. Response IDs deduplicate active/archive copies.
- Legacy `event_msg` / `token_count`: successive `total_token_usage` counters become deltas within each session. The first record uses only `last_token_usage`, never an assumed historical cumulative total. Counter decreases use the reported last usage and produce a coverage warning. Identical cumulative notifications contribute nothing again.
- If a session has modern response records, all legacy events for it are excluded. This deliberately conservative policy avoids double counting a format migration but can omit older legacy-only usage in that session. The excluded count is shown.
- `session_meta.cwd`, overridden by `turn_context.cwd`, supplies the exact project group. Unknown projects stay “Unattributed.” Sessions explicitly naming a non-OpenAI provider are excluded; missing provider metadata is accepted as a Codex log with unknown provider.
- Forked session history before the recorded fork creation time is excluded. Fork records without a creation time are skipped. Conflicting copies of a modern record ID are excluded and reported.
- Input already includes cached input. Output already includes reasoning output. `input + output` is the total; cached/reasoning subsets are never added again. Missing cache data is “Unknown.” This is not money, unique words, account-wide usage, or a subscription allowance.
- Daily buckets use UTC. No records for a day/range means unknown, not proven zero. Bars only show days with records. Future-dated records are excluded from current reports.
- **Dot and cloud usage are unmeasured**, displayed separately, and never included. This application does not probe cloud databases or private account endpoints.

Coverage lists malformed/oversized lines, unreadable entries, incomplete trailing lines, skipped usage, counter resets, unknown baselines, mixed formats, duplicate records, and inherited fork records. Incomplete last lines are retried on the next scan. Symlinks are skipped; nested traversal is capped at 12 levels and each line at 8 MiB. Unrecognized non-usage event types are ignored. Future schema changes may require an adapter update; the dashboard cannot establish that all usage was logged.

## Architecture and limits

`src/ingest.rs` streams JSONL and borrows opaque event payloads so message content is not retained. It holds normalized usage candidates in memory, sorts them, and deduplicates across files. `src/report.rs` computes range summaries. `src/main.rs` serves embedded assets and a cached JSON snapshot using `tiny_http`; a background thread scans local files. The responsive UI uses plain JavaScript and SVG with an accessible values table and no remote assets.

The v1 rescans all retained logs; very large histories can cost time and memory on smaller Pis. Use an exported snapshot or a dedicated Codex home containing selected logs when needed. Snapshot files are not cryptographically authenticated; import only files you trust. The read-only HTTP API is `/api/usage?days=7`, `30`, `90`, or `all`; `/health` checks the process, not data availability. Only GET is supported.

## Verify

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Use `--offline` if dependencies are already cached. Tests cover modern and cumulative deduplication, counter resets, missing values, mixed formats, fork history, conflicting IDs, invalid/truncated records, missing sources, UTC boundaries, project sorting, cache semantics, import provenance, non-overwriting exports, and HTTP routes. `fixtures/demo.jsonl` is entirely fictional; demo dates shift to the current UTC week.

## Pi service workflow (prepared, not deployed)

Read-only checks on 30 October 2026 found the existing dashboard at `http://192.168.0.25:9999/` reporting Raspberry Pi 5, Debian 13, ARM64, and systemd-managed services. One TCP connection to port 9988 was refused: no reachable listener was observed, but host-side port availability must be checked again before installation. The existing `Daniel/live` repository documents the user `danutz` and `/home/danutz/Development/live`; none of its files or services are changed here.

Convenience targets:

```sh
make rebuild  # release build only; no installation or service restart
make check    # format, clippy, tests
make demo     # local, labeled fictional sample
```

The proposed separate service is **token-grove.service**, at `http://192.168.0.25:9988/`. Before deployment, confirm access to the Pi, the checkout location (suggested `/home/danutz/Development/pi-usage`), and how fresh usage exports will arrive there. No GitHub repository or remote is created by this project. Establish the desired private repository/clone separately; exports are ignored by Git and must not be committed.

Review `deploy/token-grove.service.template` and replace `__PI_USER__`, `__PROJECT_DIR__`, and `__EXPORT_PATH__` with verified values. The import file must be readable by that user. This template intentionally binds to the LAN on port 9988; anyone who can reach it can view project paths and usage. It has no authentication. For a loopback-only deployment, change its bind address and adapt the update script’s explicit address guard.

The exact first-install action, **requiring a separate deployment go-ahead**, is: transfer/clone the source into the agreed new Pi directory, build and test it with the Pi's existing toolchain, transfer the chosen usage-only export, install the completed template as `/etc/systemd/system/token-grove.service`, run `systemctl daemon-reload`, then `systemctl enable --now token-grove.service` and verify `/health` and the dashboard. No changes to `live.service` or port 9999 are needed. If prerequisites such as Rust or authorized remote access are missing, resolve those separately instead of silently installing software or creating credentials.

Once that new unit is installed and its Git checkout has a configured upstream, the explicit Pi-side update workflow is:

```sh
cd /home/danutz/Development/pi-usage
make deploy SERVICE=token-grove.service
```

This refuses macOS/root execution, unknown service names, a dirty checkout, a missing upstream, or a unit that points to a different directory/binary/port. It then pulls fast-forward only, tests, builds, restarts only the named Token Grove service, and checks port 9988. It never installs the unit or changes network settings. A failed test/build leaves the running service untouched; a failed restart/health check stops with an error for investigation. Updating an import requires a new export and an explicit restart; automatic computer-to-Pi synchronization is not configured.
# pi-usage
