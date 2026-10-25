#!/usr/bin/env bash
# Creates the dev stack's local secrets in deploy/dev/.secrets (gitignored)
# and writes dev.env from them. Safe to rerun: secrets that exist are kept,
# and dev.env is rewritten so new variables appear.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
dir="$here/.secrets"
mkdir -p "$dir"
chmod 700 "$dir"

rand() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }
b64url() { base64 -w0 | tr '+/' '-_' | tr -d '='; }

# Postgres password. Older dev.env files hold it inline; keep that value.
if [[ ! -f "$dir/pg-password" ]]; then
  old="$(sed -n 's/^POSTGRES_PASSWORD=//p' "$dir/dev.env" 2>/dev/null || true)"
  printf '%s' "${old:-$(rand)}" > "$dir/pg-password"
fi

# EMQX REST API key for the hub. Bootstrap format: key:secret:role
if [[ ! -f "$dir/api-keys.txt" ]]; then
  printf 'agmen-hub:%s:administrator\n' "$(rand)" > "$dir/api-keys.txt"
fi

# Dev ES256 signing key for broker tokens, and the JWKS that EMQX reads.
# Tests sign with the key until the hub serves its own JWKS (MVP Task 7).
if [[ ! -f "$dir/jwt-dev.pem" ]]; then
  openssl ecparam -name prime256v1 -genkey -noout \
    | openssl pkcs8 -topk8 -nocrypt -out "$dir/jwt-dev.pem"
  rm -f "$dir/jwks.json"
fi
if [[ ! -f "$dir/jwks.json" ]]; then
  # A P-256 SubjectPublicKeyInfo ends with the 64 bytes of x then y.
  pub="$(openssl ec -in "$dir/jwt-dev.pem" -pubout -outform DER 2>/dev/null | od -An -v -tx1 | tr -d ' \n')"
  point="${pub: -128}"
  x="$(printf '%s' "${point:0:64}" | xxd -r -p | b64url)"
  y="$(printf '%s' "${point:64:64}" | xxd -r -p | b64url)"
  printf '{"keys":[{"kty":"EC","crv":"P-256","alg":"ES256","use":"sig","kid":"dev-1","x":"%s","y":"%s"}]}\n' "$x" "$y" > "$dir/jwks.json"
fi

# Dev CA and a localhost certificate for the HAProxy TLS front (WSS on host port 18443).
if [[ ! -f "$dir/dev-ca.pem" ]]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
    -subj "/CN=agmen dev CA" -keyout "$dir/dev-ca.key" -out "$dir/dev-ca.pem" 2>/dev/null
  rm -f "$dir/haproxy.pem"
fi
if [[ ! -f "$dir/haproxy.pem" ]]; then
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -subj "/CN=localhost" -keyout "$dir/localhost.key" -out "$dir/localhost.csr" 2>/dev/null
  openssl x509 -req -in "$dir/localhost.csr" -CA "$dir/dev-ca.pem" -CAkey "$dir/dev-ca.key" \
    -CAcreateserial -days 825 -out "$dir/localhost.pem" \
    -extfile <(printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n') 2>/dev/null
  cat "$dir/localhost.pem" "$dir/localhost.key" > "$dir/haproxy.pem"
  rm -f "$dir/localhost.csr"
fi

pg_pass="$(cat "$dir/pg-password")"
api_secret="$(cut -d: -f2 "$dir/api-keys.txt")"
cat > "$dir/dev.env" <<ENV
POSTGRES_USER=agmen
POSTGRES_DB=agmen
POSTGRES_PASSWORD=$pg_pass
AGMEN_IT_PG=postgres://agmen:$pg_pass@localhost:5432/agmen
AGMEN_IT_MQTT=mqtt://localhost:1883
AGMEN_IT_MQTT_PROXIED=mqtt://localhost:21883
AGMEN_IT_MQTT_WS=ws://localhost:8083/mqtt
AGMEN_IT_MQTT_WSS=wss://localhost:18443/mqtt
AGMEN_IT_DEV_CA=$dir/dev-ca.pem
AGMEN_IT_EMQX_API=http://localhost:18083/api/v5
AGMEN_IT_EMQX_API_KEY=agmen-hub
AGMEN_IT_EMQX_API_SECRET=$api_secret
AGMEN_IT_TOXIPROXY=http://localhost:8474
AGMEN_IT_JWT_KEY=$dir/jwt-dev.pem
AGMEN_IT_JWT_KID=dev-1
AGMEN_IT_COMPOSE_FILE=$here/compose.yaml
ENV

# EMQX (uid 1000), nginx and HAProxy read these inside their containers.
chmod 644 "$dir/api-keys.txt" "$dir/jwks.json" "$dir/haproxy.pem"
chmod 600 "$dir/dev.env" "$dir/pg-password" "$dir/jwt-dev.pem" "$dir/dev-ca.key" "$dir/localhost.key"
echo "dev secrets ready in $dir"
