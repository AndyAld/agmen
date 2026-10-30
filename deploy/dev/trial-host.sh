#!/usr/bin/env bash
# Save the trial hub's public host name in the gitignored trial-hub.env, once.
# Usage: trial-host.sh [host]. With no host it asks on the terminal.
set -euo pipefail

env_file="${AGMEN_TRIAL_HUB_ENV:-/Users/andyaldrich/Projects/agmen/deploy/dev/.secrets/trial-hub.env}"
host_re='^[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$'

host="${1-}"
if (($# == 0)); then
  if [[ ! -t 0 ]]; then
    printf 'trial-host.sh: no terminal to ask on; run deploy/dev/trial-host.sh <host>\n' >&2
    exit 2
  fi
  printf 'Trial hub host name for AGMEN_TRIAL_HOST (such as machine.tailnet.ts.net): ' >&2
  IFS= read -r -n 253 host || true
fi
if [[ ! "$host" =~ $host_re ]]; then
  printf 'trial-host.sh: AGMEN_TRIAL_HOST must be a host name, such as machine.tailnet.ts.net\n' >&2
  exit 1
fi

if [[ -L "$env_file" || (-e "$env_file" && ! -f "$env_file") ]]; then
  printf 'trial-host.sh: %s is not a regular file\n' "$env_file" >&2
  exit 1
fi
mkdir -p "$(dirname "$env_file")"
umask 077
tmp_file="$(mktemp "$env_file.XXXXXX")"
trap 'rm -f "$tmp_file"' EXIT
if [[ -f "$env_file" ]]; then
  # Keep every other line; drop any earlier AGMEN_TRIAL_HOST.
  { grep -v '^AGMEN_TRIAL_HOST=' "$env_file" || true; } >"$tmp_file"
fi
printf "AGMEN_TRIAL_HOST='%s'\n" "$host" >>"$tmp_file"
chmod 600 "$tmp_file"
mv -f "$tmp_file" "$env_file"
trap - EXIT
printf 'AGMEN_TRIAL_HOST is %s\n' "$host"
