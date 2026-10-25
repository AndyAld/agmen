#!/usr/bin/env bash
# Creates the dev stack's local secrets in deploy/dev/.secrets (gitignored).
# Safe to rerun: existing files are kept.
set -euo pipefail

dir="$(cd "$(dirname "$0")" && pwd)/.secrets"
mkdir -p "$dir"
chmod 700 "$dir"

rand() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

if [[ ! -f "$dir/api-keys.txt" ]]; then
  # EMQX bootstrap format: key:secret:role
  printf 'agmen-hub:%s:administrator\n' "$(rand)" > "$dir/api-keys.txt"
fi

if [[ ! -f "$dir/dev.env" ]]; then
  pg_pass="$(rand)"
  api_secret="$(cut -d: -f2 "$dir/api-keys.txt")"
  cat > "$dir/dev.env" <<ENV
POSTGRES_USER=agmen
POSTGRES_DB=agmen
POSTGRES_PASSWORD=$pg_pass
AGMEN_IT_PG=postgres://agmen:$pg_pass@localhost:5432/agmen
AGMEN_IT_MQTT=mqtt://localhost:1883
AGMEN_IT_MQTT_PROXIED=mqtt://localhost:21883
AGMEN_IT_MQTT_WS=ws://localhost:8083/mqtt
AGMEN_IT_EMQX_API=http://localhost:18083/api/v5
AGMEN_IT_EMQX_API_KEY=agmen-hub
AGMEN_IT_EMQX_API_SECRET=$api_secret
AGMEN_IT_TOXIPROXY=http://localhost:8474
ENV
fi

# EMQX runs as uid 1000 inside the container and must read the key file.
chmod 644 "$dir/api-keys.txt"
chmod 600 "$dir/dev.env"
echo "dev secrets ready in $dir"
