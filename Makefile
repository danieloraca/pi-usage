.PHONY: help rebuild check test run demo deploy

help:
	@printf '%s\n' 'make rebuild  Build the release binary' 'make check    Format, lint, and test' 'make run      Serve local Codex usage on loopback' 'make demo     Serve labeled sample data' 'make deploy SERVICE=token-grove.service  Explicit Pi-side update of an already installed service'

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
