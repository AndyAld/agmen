#!/usr/bin/env bash
# Exercise the trial host setter on a temporary env file.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
env_file="$tmp/trial.env"
set_host() {
  env -i PATH="$PATH" HOME="$tmp" AGMEN_TRIAL_HUB_ENV="$env_file" \
    bash "$root/deploy/dev/trial-host.sh" "$@" </dev/null
}

# A new file is created owner-only with the one line.
set_host first.example.ts.net >"$tmp/out"
[[ "$(cat "$env_file")" == "AGMEN_TRIAL_HOST='first.example.ts.net'" ]]
[[ "$(stat -f '%Lp' "$env_file")" == 600 ]]
rg -q '^AGMEN_TRIAL_HOST is first.example.ts.net$' "$tmp/out"

# A rerun replaces the host and keeps every other line.
printf 'KEEP_ME=1\n' >>"$env_file"
set_host second.example.ts.net >/dev/null
[[ "$(rg -c '^AGMEN_TRIAL_HOST=' "$env_file")" == 1 ]]
rg -q "^AGMEN_TRIAL_HOST='second.example.ts.net'$" "$env_file"
rg -q '^KEEP_ME=1$' "$env_file"
[[ "$(stat -f '%Lp' "$env_file")" == 600 ]]

# Bad hosts are refused and change nothing.
before="$(cat "$env_file")"
for bad in '' 'host with space' 'evil.example/path' '-flag.example' "x';touch $tmp/pwned;'"; do
  if set_host "$bad" >"$tmp/out" 2>&1; then
    printf 'accepted trial host %q\n' "$bad" >&2
    exit 1
  fi
  rg -q 'AGMEN_TRIAL_HOST' "$tmp/out"
done
[[ "$(cat "$env_file")" == "$before" ]]
[[ ! -e "$tmp/pwned" ]]

# With no host and no terminal to ask on, it exits 2 and says how to pass one.
if set_host >"$tmp/out" 2>&1; then
  echo 'accepted a missing host off a terminal' >&2
  exit 1
else
  [[ $? == 2 ]]
fi
rg -q 'trial-host.sh <host>' "$tmp/out"

# A symlink in place of the env file is refused.
rm "$env_file"
ln -s "$tmp/elsewhere" "$env_file"
if set_host third.example.ts.net >"$tmp/out" 2>&1; then
  echo 'wrote through a symlink' >&2
  exit 1
fi
[[ ! -e "$tmp/elsewhere" ]]
printf 'trial host tests passed\n'
