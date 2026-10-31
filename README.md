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

Live sources are rescanned every 60 seconds (`--refresh SECONDS`, minimum 5); imported files are checked every five seconds by default. The browser polls the cached report every 60 seconds; Refresh view fetches that report immediately. It does not force a log rescan. Source timestamps are visible. Stop the service with Ctrl-C.

## Starting again after shutting down both machines

Once first-time setup is complete (`token-grove.service` is enabled on the Pi and `sync-install` has succeeded on the Mac), turn both machines on:

- **Pi:** the systemd service starts during boot. No user login is required.
- **Mac:** log into your account. The user LaunchAgent starts at login and checks for changed usage every five minutes. It does not run before login.
- **Either boot order works:** if the Pi is not reachable yet, the Mac retries at the next scheduled run. Until a successful transfer, the dashboard keeps showing its last saved snapshot. Its “Usage snapshot” timestamp indicates data freshness; a reachable dashboard alone does not prove sync is working.

Open `http://<pi-address>:9988/` after the Pi has joined the network. No manual restart command is normally needed.

For a manual restart **on the Pi**:

```sh
sudo systemctl restart token-grove.service
systemctl status token-grove.service --no-pager
```

For a manual restart **on the Mac**:

```sh
cd ~/Daniel/pi-usage
make sync-restart
make sync-status CONFIG="$HOME/Library/Application Support/Token Grove/sync.json"
```

`sync-restart` uses the already installed agent and does not require `CONFIG`. If it says the agent is not loaded, follow the Mac setup below and run `sync-install` with the config path. Use `systemctl is-enabled token-grove.service` on the Pi to check boot startup. Both machines need working network access, and Mac-to-Pi sync needs SSH authentication without password prompts.

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

Exports contain token counts, timestamps, session/response identifiers, project working-directory paths, and coverage diagnostics. They contain **no prompts, responses, tool bodies, or credentials**. Treat project names and paths as private. Export refuses to replace an existing file; use a new filename. Imports accept schema version 1, validate counts, deduplicate identifiers, preserve the demo label and original snapshot timestamp, and reject files over 64 MiB. Imports are checked every five seconds (override with `--refresh`); changed files are validated and replace the in-memory snapshot without a restart. Invalid, incomplete, or older updates leave the last valid snapshot visible with a reload warning. An invalid initial import still fails startup clearly.

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

The prepared deployment targets a Raspberry Pi running Linux ARM64 with systemd. Check the intended host, service user, Rust toolchain, and availability of port 9988 before installing. The service is separate from any existing dashboard on port 9999. ARM hardware validation is still outstanding.

Convenience targets:

```sh
make rebuild  # release build only; no installation or service restart
make check    # format, clippy, tests
make demo     # local, labeled fictional sample
```

The proposed separate service is **token-grove.service**, at `http://<pi-address>:9988/`. Before deployment, confirm access to the Pi, the checkout location (for example `~/Development/pi-usage`), and how fresh usage exports will arrive there. Clone the [source repository](https://github.com/danieloraca/pi-usage) into that new directory. Usage exports are private data: they are ignored by Git and must not be committed.

Review `deploy/token-grove.service.template` and replace `__PI_USER__`, `__PROJECT_DIR__`, and `__EXPORT_PATH__` with verified values. The import file must be readable by that user. This template intentionally binds to the LAN on port 9988; anyone who can reach it can view project paths and usage. It has no authentication. For a loopback-only deployment, change its bind address and adapt the update script’s explicit address guard.

The exact first-install action, **requiring a separate deployment go-ahead**, is: transfer/clone the source into the agreed new Pi directory, build and test it with the Pi's existing toolchain, transfer the chosen usage-only export, install the completed template as `/etc/systemd/system/token-grove.service`, run `systemctl daemon-reload`, then `systemctl enable --now token-grove.service` and verify `/health` and the dashboard. No changes to `live.service` or port 9999 are needed. If prerequisites such as Rust or authorized remote access are missing, resolve those separately instead of silently installing software or creating credentials.

Once that new unit is installed and its Git checkout has a configured upstream, the explicit Pi-side update workflow is:

```sh
cd ~/Development/pi-usage
make deploy SERVICE=token-grove.service
```

This refuses macOS/root execution, unknown service names, a dirty checkout, a missing upstream, or a unit that points to a different directory/binary/port. It then pulls fast-forward only, tests, builds, restarts only the named Token Grove service, and checks port 9988. It never installs the unit or changes network settings. A failed test/build leaves the running service untouched; a failed restart/health check stops with an error for investigation. New imports reload automatically after validation. Computer-to-Pi synchronization is opt-in; see the Mac setup below.

# pi-usage

## Optional Mac background sync

The same Rust binary can export and send usage-only snapshots over your **existing** SSH access. This feature is implemented but is not enabled by building, running the dashboard, or pulling the repository. No SSH keys, host-trust entries, or authentication settings are created or changed.

First update and restart the Pi service once with this version, so its binary supports `receive` and its server supports import reload. The SSH user must be able to execute that binary and write the import file's parent directory. The dashboard's systemd sandbox can remain read-only. Keep `remote_snapshot` exactly equal to the service's `--import` path.

On the Mac, prepare a private config and replace every `YOUR_...` placeholder with verified values. Paths must be absolute; JSON does not expand `~` or environment variables:

```sh
cd /path/to/pi-usage
install -d -m 700 "$HOME/Library/Application Support/Token Grove"
install -m 600 deploy/sync-config.example.json "$HOME/Library/Application Support/Token Grove/sync.json"
# Edit that JSON file with your editor, then:
make rebuild
make sync-dry-run CONFIG="$HOME/Library/Application Support/Token Grove/sync.json"
```

The dry run performs local metadata checks and, when needed, a private export. It never calls SSH or marks data as successfully delivered. Check the generated LaunchAgent without installing it:

```sh
make -s sync-plist CONFIG="$HOME/Library/Application Support/Token Grove/sync.json" > /tmp/token-grove-sync.plist
plutil -lint /tmp/token-grove-sync.plist
```

**Activation is a separate explicit action.** Once the destination and existing noninteractive SSH access are confirmed, these commands send real usage metadata and enable persistent background work, respectively:

```sh
make sync-once CONFIG="$HOME/Library/Application Support/Token Grove/sync.json"
make sync-install CONFIG="$HOME/Library/Application Support/Token Grove/sync.json"
make sync-status CONFIG="$HOME/Library/Application Support/Token Grove/sync.json"
```

`sync-install` installs a stable copy of the release binary at `~/Library/Application Support/Token Grove/bin/pi-usage` and the user LaunchAgent at `~/Library/LaunchAgents/com.tokengrove.sync.plist`. It starts at load and on each clock minute divisible by five, at background priority with low-priority I/O. Re-run installation after rebuilding to update the installed Mac binary. It is a logged-in user's agent, not a root daemon. No growing log file is created; the private state directory holds one overwritten `status.json` and one successful-sync marker.

The calendar schedule coalesces missed sleep intervals into one invocation after wake, rather than replaying a backlog. It does not wake the Mac or keep it awake. At login `RunAtLoad` checks the latest state. See [Apple's launchd scheduling documentation](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html). If the Pi is offline after wake, the next five-minute run retries.

After a Mac restart, the installed LaunchAgent starts automatically when you log in; there is no manual restart step. It does not run before login. For a manual restart of an already loaded agent:

```sh
cd /path/to/pi-usage
make sync-restart
```

This restarts the installed sync worker immediately and keeps its five-minute schedule. It uses the existing installed config and binary, so no `CONFIG` argument is needed. It refuses to run on Linux or when the agent is not loaded. To install the agent or update its binary after code changes, use `make sync-install CONFIG=...` instead. A restart request is not proof of a successful transfer; use `make sync-status CONFIG=...` to check the outcome.

To disable background work:

```sh
make sync-uninstall
```

This unloads and removes only Token Grove's LaunchAgent. Config, private state, and the installed binary are retained. Uninstall works even when the config was removed or damaged. No Pi service is changed by install/uninstall.

### Work limits and recovery

- A kernel file lock prevents concurrent runs using the same `state_dir`. It releases on process exit, including crashes; the lock file stays in place. Use one state directory for this Mac sender. A separate receiver lock serializes atomic replacements on the Pi.
- Idle intervals stat the source tree and compare a SHA-256 digest of file identity, size, modification/change times, and paths. They do not read log bodies, export, or connect to SSH. A changed tree invokes the existing Rust exporter. A second SHA-256 comparison of canonical usage and coverage ignores export time, so prompt-only changes with identical usage/coverage cause no transfer.
- The scan is bounded to 5,000 JSONL logs, 4 GiB of source files, 20,000 visited entries, and 12 directory levels. Changed-source exports are limited to 120 seconds by default (configurable up to 180). SSH has a 10-second connection timeout and a 60-second overall timeout by default (maximum 90). Timed-out subprocess groups are stopped and reaped. Filesystems themselves must respond normally; these budgets are not hard real-time guarantees for blocked OS I/O.
- Active appends may change logs during export. A validated snapshot is still sent with a coverage note, and its source is deliberately left uncached so the next run rescans. Missing, empty, symlinked, or unreadable source trees are refused; they never erase the Pi's prior data. No conversation/tool content is exported.
- Each transfer is capped at 64 MiB. SSH runs without a terminal, password prompts, agent/X11 forwarding, connection persistence, or host-key changes. `StrictHostKeyChecking=yes` requires trust to exist already. Failed authentication or an unknown host is an error, not an invitation to create access automatically.
- The Pi `receive` command reads stdin, validates schema/counts/IDs, strips unknown fields, rejects older snapshots, and atomically renames a synced temporary file in the same directory. Files are mode 0600. Invalid/truncated input leaves the old file untouched. Matching usage is idempotent. The server swaps a complete validated snapshot; it never appends records, so reloads cannot double count.
- Only a successful SSH exit advances `success.json`. A timeout after the Pi accepted a file is safe: the next run retries idempotently. Failures retain the last good Pi snapshot. `sync-status` reports the last attempt and last successful transfer. If local state is corrupted, move `success.json` aside and retry; this forces a full validated resend. Also do this after manually deleting/replacing the Pi's data: idle runs deliberately do not probe remote state.
- Config/state must belong to the current user. Config and state files require private permissions (0600), and the state directory requires 0700; symlinked lock/config files are refused. State/temp files stay private and successful runs clean their export/transfer files. A later run cleans named leftover transfer temporaries after an interrupted run.

The API adds `reload.last_checked`, `reload.last_loaded`, `reload.reloads`, and `reload.error` for imported sources. The browser distinguishes a failed update from the retained valid snapshot and continues polling once a minute. “Usage snapshot” is when that data was captured, not proof of recent activity; unchanged usage is intentionally not retransmitted. Dot/cloud usage remains unmeasured.

### Sync validation

`cargo test` includes fake-transport tests for unchanged-source/usage skips, changed data, destination changes, offline retry, active appends, overlapping runs, dry-run isolation, private permissions, subprocess timeouts, malformed/conflicting/older snapshot rejection, deduplication, and actual HTTP reload without a restart. An end-to-end local dry run exercises the real exporter against fictional logs containing a message that must not appear in the export. No test activates launchd or contacts the Pi.

The `idle_fast_path_reports_metadata_only_cost` test prints timing for ten runs over 266 fixture log files. Use this to compare overhead on a particular machine; it is not a Pi performance claim. Run `cargo test --release --test sync idle_fast_path -- --nocapture` for the optimized benchmark.
