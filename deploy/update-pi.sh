#!/usr/bin/env bash
set -euo pipefail

# This script only updates an existing, separately installed Token Grove service.
# It never installs units, creates users, changes firewalls, or touches live.service.
cd "$(dirname "$0")/.."
project_dir="$(pwd -P)"
: "${SERVICE:?Set SERVICE to the installed Token Grove unit, e.g. token-grove.service}"
if [[ "$SERVICE" != token-grove.service && "$SERVICE" != pi-usage.service ]]; then
  echo 'Only token-grove.service or pi-usage.service may be updated by this project.' >&2
  exit 1
fi
if [[ "$(uname -s)" != Linux ]]; then
  echo 'Run make deploy in the Pi checkout. Use make rebuild on this computer.' >&2
  exit 1
fi
if [[ "$EUID" == 0 ]]; then
  echo 'Run as the normal project owner; only service restart uses sudo.' >&2
  exit 1
fi
for tool in git cargo systemctl curl sudo; do command -v "$tool" >/dev/null; done
git rev-parse --is-inside-work-tree >/dev/null
if [[ -n "$(git status --porcelain)" ]]; then
  echo 'Working tree is not clean. Commit or set aside changes before deployment.' >&2
  exit 1
fi
git rev-parse --verify '@{upstream}' >/dev/null
if [[ "$(systemctl show "$SERVICE" --property=LoadState --value)" != loaded ]]; then
  echo 'The Token Grove unit must be reviewed and installed separately first.' >&2
  exit 1
fi
if [[ "$(systemctl show "$SERVICE" --property=WorkingDirectory --value)" != "$project_dir" ]]; then
  echo 'Service WorkingDirectory does not match this checkout; refusing restart.' >&2
  exit 1
fi
service_command="$(systemctl show "$SERVICE" --property=ExecStart --value)"
if [[ "$service_command" != *"path=$project_dir/target/release/pi-usage ;"* || "$service_command" != *"--bind 0.0.0.0:9988"* ]]; then
  echo 'Service must run this checkout’s pi-usage binary on 0.0.0.0:9988.' >&2
  exit 1
fi

git pull --ff-only
cargo test --locked
cargo build --release --locked
sudo systemctl restart "$SERVICE"
systemctl is-active --quiet "$SERVICE"
for attempt in {1..10}; do
  if curl --fail --silent --show-error --max-time 2 http://127.0.0.1:9988/health >/dev/null; then
    echo 'Token Grove is responding on port 9988. Check dashboard coverage and snapshot time.'
    exit 0
  fi
  sleep 1
done
echo 'The restart completed, but the health check failed. Inspect journalctl; no automatic rollback was attempted.' >&2
exit 1
