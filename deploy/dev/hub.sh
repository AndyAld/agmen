#!/usr/bin/env bash
# Run the trial hub against the existing dev stack. Funnel stays manual.
set +x
set +v
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
AGMEN_DEV_ENV="${AGMEN_DEV_ENV:-/Users/andyaldrich/Projects/agmen/deploy/dev/.secrets/dev.env}"
AGMEN_TRIAL_HUB_ENV="${AGMEN_TRIAL_HUB_ENV:-/Users/andyaldrich/Projects/agmen/deploy/dev/.secrets/trial-hub.env}"
for name in AGMEN_DEV_ENV AGMEN_TRIAL_HUB_ENV; do
  if [[ ! -r "${!name}" ]]; then
    printf 'hub.sh: cannot read %s\n' "$name" >&2
    exit 1
  fi
done

# Suppress shell diagnostics as they can contain an assignment's value.
set -a
# shellcheck disable=SC1090
if ! source "$AGMEN_DEV_ENV" >/dev/null 2>&1; then
  printf 'hub.sh: cannot source AGMEN_DEV_ENV\n' >&2
  exit 1
fi
# shellcheck disable=SC1090
if ! source "$AGMEN_TRIAL_HUB_ENV" >/dev/null 2>&1; then
  printf 'hub.sh: cannot source AGMEN_TRIAL_HUB_ENV\n' >&2
  exit 1
fi

# init.sh writes integration-test names, not the hub's configuration names.
AGMEN_DATABASE_URL="${AGMEN_DATABASE_URL:-${AGMEN_IT_PG:-}}"
AGMEN_MQTT_URL="${AGMEN_MQTT_URL:-${AGMEN_IT_MQTT:-}}"
AGMEN_EMQX_API_URL="${AGMEN_EMQX_API_URL:-${AGMEN_IT_EMQX_API:-}}"
AGMEN_EMQX_API_KEY="${AGMEN_EMQX_API_KEY:-${AGMEN_IT_EMQX_API_KEY:-}}"
AGMEN_EMQX_API_SECRET="${AGMEN_EMQX_API_SECRET:-${AGMEN_IT_EMQX_API_SECRET:-}}"
if [[ -z "${AGMEN_JWT_SIGNING_KEY:-}" && -n "${AGMEN_IT_JWT_DIR:-}" ]]; then
  AGMEN_JWT_SIGNING_KEY="$AGMEN_IT_JWT_DIR/agmen-dev-1.pem"
fi

# The trial's public host (Tailscale Funnel), kept out of the repository:
# set AGMEN_TRIAL_HOST in AGMEN_TRIAL_HUB_ENV, such as <machine>.<tailnet>.ts.net.
AGMEN_TRIAL_HOST="${AGMEN_TRIAL_HOST:-}"
if [[ -z "$AGMEN_TRIAL_HOST" && -t 0 ]]; then
  # First run on a terminal: ask once and save it in AGMEN_TRIAL_HUB_ENV.
  saved="$(AGMEN_TRIAL_HUB_ENV="$AGMEN_TRIAL_HUB_ENV" bash "$root/deploy/dev/trial-host.sh")"
  AGMEN_TRIAL_HOST="${saved#AGMEN_TRIAL_HOST is }"
fi
if [[ -n "$AGMEN_TRIAL_HOST" && ! "$AGMEN_TRIAL_HOST" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$ ]]; then
  printf 'hub.sh: AGMEN_TRIAL_HOST must be a host name, such as machine.tailnet.ts.net\n' >&2
  exit 1
fi

# These must describe the trial, even when a dev shell has local overrides.
export AGMEN_LISTEN=127.0.0.1:8080
AGMEN_PUBLIC_URL=
AGMEN_PUBLIC_MQTT_URL=
if [[ -n "$AGMEN_TRIAL_HOST" ]]; then
  AGMEN_PUBLIC_URL="https://$AGMEN_TRIAL_HOST"
  AGMEN_PUBLIC_MQTT_URL="wss://$AGMEN_TRIAL_HOST:8443/mqtt"
fi
export AGMEN_PUBLIC_URL AGMEN_PUBLIC_MQTT_URL
export AGMEN_GITHUB_URL=https://github.com
export AGMEN_GITHUB_API_URL=https://api.github.com
set +a
export AGMEN_JWT_SIGNING_KEY

required=(
  AGMEN_TRIAL_HOST AGMEN_DATABASE_URL AGMEN_LISTEN AGMEN_PUBLIC_URL
  AGMEN_GITHUB_CLIENT_ID AGMEN_GITHUB_CLIENT_SECRET
  AGMEN_EMQX_API_URL AGMEN_EMQX_API_KEY AGMEN_EMQX_API_SECRET
  AGMEN_MQTT_URL AGMEN_PUBLIC_MQTT_URL AGMEN_JWT_SIGNING_KEY
)
missing=()
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    missing+=("$name")
  fi
done
if ((${#missing[@]})); then
  printf 'hub.sh: missing required variables:\n' >&2
  printf '  %s\n' "${missing[@]}" >&2
  exit 1
fi
if [[ ! -r "$AGMEN_JWT_SIGNING_KEY" ]]; then
  printf 'hub.sh: AGMEN_JWT_SIGNING_KEY is not a readable file\n' >&2
  exit 1
fi

printf 'Trial hub: %s\n' "$AGMEN_PUBLIC_URL"
cat <<'HELP'
Local API: 127.0.0.1:8080. Stop the hub with Ctrl-C.

In another terminal, Andy exposes the API and MQTT WebSocket listener:
  tailscale funnel --bg --https=443 http://127.0.0.1:8080
  tailscale funnel --bg --https=8443 http://127.0.0.1:8083
Turn those listeners off separately (Ctrl-C does not stop background Funnel):
  tailscale funnel --https=443 off
  tailscale funnel --https=8443 off

Friends need Rust 1.99.1 and access to the source repository. From its checkout:
  cargo install --locked --path crates/agmen-cli
Or install from the repository (repository access required):
  cargo install --locked --git https://git.groupdrive.xyz/groupdrive/agmen agmen-cli
HELP
printf '  export AGMEN_HUB=%s\n' "$AGMEN_PUBLIC_URL"
cat <<'HELP'
  agmen login
Open the URL printed by login and sign in with GitHub.
Hub selection: login --hub overrides AGMEN_HUB, then the last saved login.

Andy signs in to the same hub and runs agmen invite in a joined worktree,
or agmen invite --project <id> (agmen whoami lists projects).
A project and its first admin must already exist; see agmen-hub bootstrap.
Make a separate single-use invite for each friend. In a local git worktree:
  agmen join <code>
Then run agmen watch or agmen post FYI 'hello'.
HELP

cd "$root"
# exec leaves terminal signals with cargo and the hub; no shell-owned child.
exec cargo run --locked --release -p agmen-hub -- serve
