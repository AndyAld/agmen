#!/usr/bin/env bash
# Exercise the launcher without starting a hub or reading real credentials.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
cat >"$tmp/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'run --locked --release -p agmen-hub -- serve' ]]
[[ "$AGMEN_LISTEN" == '127.0.0.1:8080' ]]
[[ "$AGMEN_PUBLIC_URL" == 'https://trial-host.example.ts.net' ]]
[[ "$AGMEN_PUBLIC_MQTT_URL" == 'wss://trial-host.example.ts.net:8443/mqtt' ]]
[[ "$AGMEN_GITHUB_URL" == 'https://github.com' ]]
[[ "$AGMEN_GITHUB_API_URL" == 'https://api.github.com' ]]
[[ "$AGMEN_DATABASE_URL" == 'synthetic-database' ]]
[[ "$AGMEN_MQTT_URL" == 'mqtt://localhost:1883' ]]
[[ "$AGMEN_EMQX_API_URL" == 'http://localhost:18083/api/v5' ]]
[[ "$AGMEN_EMQX_API_KEY" == 'synthetic-api-key' ]]
[[ "$AGMEN_EMQX_API_SECRET" == 'synthetic-api-secret' ]]
[[ "$AGMEN_GITHUB_CLIENT_ID" == 'synthetic-client-id' ]]
[[ "$AGMEN_GITHUB_CLIENT_SECRET" == 'synthetic-client-secret' ]]
[[ "$AGMEN_JWT_SIGNING_KEY" == "$TEST_JWT/agmen-dev-1.pem" ]]
printf 'cargo stub passed\n'
CARGO
chmod +x "$tmp/bin/cargo"
mkdir "$tmp/jwt"
touch "$tmp/jwt/agmen-dev-1.pem"
cat >"$tmp/dev.env" <<ENV
AGMEN_IT_PG=synthetic-database
AGMEN_IT_MQTT=mqtt://localhost:1883
AGMEN_IT_EMQX_API=http://localhost:18083/api/v5
AGMEN_IT_EMQX_API_KEY=synthetic-api-key
AGMEN_IT_EMQX_API_SECRET=synthetic-api-secret
AGMEN_IT_JWT_DIR='$tmp/jwt'
AGMEN_LISTEN=0.0.0.0:9999
AGMEN_PUBLIC_URL=http://localhost:9999
AGMEN_PUBLIC_MQTT_URL=mqtt://localhost:1883
AGMEN_GITHUB_URL=http://fake-github
AGMEN_GITHUB_API_URL=http://fake-github
ENV
write_trial_env() {
  cat >"$tmp/trial.env" <<ENV
AGMEN_GITHUB_CLIENT_ID=synthetic-client-id
AGMEN_GITHUB_CLIENT_SECRET=synthetic-client-secret
AGMEN_TRIAL_HOST='$1'
ENV
}
write_trial_env trial-host.example.ts.net
run() {
  env -i PATH="$tmp/bin:$PATH" HOME="$tmp" TEST_JWT="$tmp/jwt" \
    AGMEN_DEV_ENV="$tmp/dev.env" AGMEN_TRIAL_HUB_ENV="$tmp/trial.env" \
    bash "$@" "$root/deploy/dev/hub.sh"
}
run >"$tmp/out" 2>&1
rg -q '^cargo stub passed$' "$tmp/out"
rg -q 'tailscale funnel --bg --https=443 http://127.0.0.1:8080' "$tmp/out"
rg -q 'tailscale funnel --bg --https=8443 http://127.0.0.1:8083' "$tmp/out"
rg -q 'tailscale funnel --https=443 off' "$tmp/out"
rg -q 'tailscale funnel --https=8443 off' "$tmp/out"
rg -q 'agmen login' "$tmp/out"
rg -q 'agmen join <code>' "$tmp/out"
rg -q '^Trial hub: https://trial-host.example.ts.net$' "$tmp/out"
rg -q 'export AGMEN_HUB=https://trial-host.example.ts.net$' "$tmp/out"
if rg -q 'synthetic-' "$tmp/out"; then
  echo 'launcher printed synthetic credentials' >&2
  exit 1
fi
run -x >"$tmp/out" 2>&1
rg -q '^cargo stub passed$' "$tmp/out"
if rg -q 'synthetic-' "$tmp/out"; then
  echo 'shell tracing printed synthetic credentials' >&2
  exit 1
fi
for bad in '' 'host with space' 'evil.example/path' '-flag.example'; do
  write_trial_env "$bad"
  if run >"$tmp/out" 2>&1; then
    printf 'launcher accepted trial host %q\n' "$bad" >&2
    exit 1
  fi
  rg -q 'AGMEN_TRIAL_HOST' "$tmp/out"
  if rg -q 'cargo stub passed' "$tmp/out"; then
    echo 'launcher started with a bad trial host' >&2
    exit 1
  fi
done
write_trial_env trial-host.example.ts.net
rm "$tmp/jwt/agmen-dev-1.pem"
if run >"$tmp/out" 2>&1; then
  echo 'launcher accepted a missing signing key' >&2
  exit 1
fi
rg -q 'AGMEN_JWT_SIGNING_KEY is not a readable file' "$tmp/out"
cat >"$tmp/trial.env" <<'ENV'
printf 'synthetic-secret-diagnostic\n' >&2
return 1
ENV
if run >"$tmp/out" 2>&1; then
  echo 'launcher accepted an env source failure' >&2
  exit 1
fi
rg -q 'cannot source AGMEN_TRIAL_HUB_ENV' "$tmp/out"
if rg -q 'synthetic-' "$tmp/out"; then
  echo 'source failure printed synthetic credentials' >&2
  exit 1
fi
: >"$tmp/dev.env"
: >"$tmp/trial.env"
if run >"$tmp/out" 2>&1; then
  echo 'launcher accepted missing configuration' >&2
  exit 1
fi
for name in AGMEN_TRIAL_HOST AGMEN_DATABASE_URL AGMEN_MQTT_URL AGMEN_EMQX_API_URL AGMEN_EMQX_API_KEY \
  AGMEN_EMQX_API_SECRET AGMEN_GITHUB_CLIENT_ID AGMEN_GITHUB_CLIENT_SECRET AGMEN_JWT_SIGNING_KEY; do
  rg -q "^  $name$" "$tmp/out"
done
if rg -q 'cargo stub passed|tailscale funnel' "$tmp/out"; then
  echo 'launcher continued after missing configuration' >&2
  exit 1
fi
rm "$tmp/trial.env"
if run >"$tmp/out" 2>&1; then
  echo 'launcher accepted a missing env file' >&2
  exit 1
fi
rg -q 'AGMEN_TRIAL_HUB_ENV' "$tmp/out"
printf 'hub launcher tests passed\n'
