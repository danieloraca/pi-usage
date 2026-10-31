.PHONY: help rebuild check test run demo deploy sync-dry-run sync-once sync-install sync-uninstall sync-status sync-plist

help:
	@printf '%s\n' 'make rebuild  Build the release binary' 'make check    Format, lint, and test' 'make run      Serve local Codex usage on loopback' 'make demo     Serve labeled sample data' 'make deploy SERVICE=token-grove.service  Explicit Pi-side update of an already installed service' 'make sync-dry-run CONFIG=/absolute/sync.json  Local checks only' 'make sync-once CONFIG=/absolute/sync.json  Send one real snapshot' 'make sync-install CONFIG=/absolute/sync.json  Enable five-minute Mac sync' 'make sync-status CONFIG=/absolute/sync.json  Show sync status' 'make sync-uninstall  Disable the Mac LaunchAgent'

rebuild:
	cargo build --release --locked

check:
	cargo fmt --all --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

test:
	cargo test --locked

run:
	cargo run --release --locked -- serve

demo:
	cargo run --release --locked -- serve --demo

# Deliberately not a dependency of build/test. Requires an explicit service name.
deploy:
	SERVICE="$(SERVICE)" bash deploy/update-pi.sh

# CONFIG must name an absolute, private (0600) JSON config; no defaults send data.
sync-dry-run:
	./target/release/pi-usage sync --config "$(CONFIG)" --dry-run

sync-once:
	./target/release/pi-usage sync --config "$(CONFIG)"

sync-plist:
	./target/release/pi-usage sync-agent render --config "$(CONFIG)"

# Explicit activation: installs/replaces the LaunchAgent and runs it at load.
sync-install: rebuild
	./target/release/pi-usage sync-agent install --config "$(CONFIG)"

sync-uninstall:
	./target/release/pi-usage sync-agent uninstall

sync-status:
	./target/release/pi-usage sync-agent status --config "$(CONFIG)"
