#!/usr/bin/env bash
# Creates the dev stack's local secrets in deploy/dev/.secrets (gitignored).
# Safe to rerun: existing files are kept, and missing ones are added.
set -euo pipefail

dir="$(cd "$(dirname "$0")" && pwd)/.secrets"
mkdir -p "$dir"
chmod 700 "$dir"

rand() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

if [[ ! -f "$dir/api-keys.txt" ]]; then
  # EMQX bootstrap format: key:secret:role
  printf 'agmen-hub:%s:administrator\n' "$(rand)" >"$dir/api-keys.txt"
fi

if [[ ! -f "$dir/dev.env" ]]; then
  pg_pass="$(rand)"
  api_secret="$(cut -d: -f2 "$dir/api-keys.txt")"
  cat >"$dir/dev.env" <<ENV
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

# Adds KEY=VALUE to dev.env unless KEY is already set there, so a rerun
# brings an older dev.env up to date without touching its values.
ensure_env() {
  if ! grep -q "^$1=" "$dir/dev.env"; then
    printf '%s=%s\n' "$1" "$2" >>"$dir/dev.env"
  fi
}

# Dev JWT signing keys (ES256, PKCS#8 PEM). The key id is the file name
# without .pem. agmen-dev-1 and agmen-dev-2 are both in the dev JWKS, so a
# rotation test can switch keys while EMQX accepts both. untrusted.pem is
# never published: tokens it signs must be refused.
mkdir -p "$dir/jwt" "$dir/jwks"
for name in agmen-dev-1 agmen-dev-2 untrusted; do
  if [[ ! -f "$dir/jwt/$name.pem" ]]; then
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
      -out "$dir/jwt/$name.pem" 2>/dev/null
  fi
  chmod 600 "$dir/jwt/$name.pem"
done

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# Public JWK for a P-256 key: the DER public key ends with 04 || x || y.
# Called inside $(...), where bash does not apply errexit, so each step
# that can fail returns 1 itself.
jwk() {
  local der size x y
  der="$(mktemp)" || return 1
  if ! openssl pkey -in "$dir/jwt/$1.pem" -pubout -outform DER -out "$der" 2>/dev/null; then
    rm -f "$der"
    echo "init.sh: cannot read the public key in $dir/jwt/$1.pem" >&2
    return 1
  fi
  size="$(wc -c <"$der" | tr -d ' ')" || return 1
  if ((size < 64)); then
    rm -f "$der"
    echo "init.sh: the public key in $dir/jwt/$1.pem is $size bytes, too short for P-256" >&2
    return 1
  fi
  x="$(tail -c 64 "$der" | head -c 32 | b64url)" || return 1
  y="$(tail -c 32 "$der" | b64url)" || return 1
  rm -f "$der"
  printf '{"kty":"EC","crv":"P-256","alg":"ES256","use":"sig","kid":"%s","x":"%s","y":"%s"}' "$1" "$x" "$y"
}

# Rebuilt on every run from the key files, so it always matches them. The
# jwks service serves this directory at http://localhost:8090/v1/jwks. Each
# JWK is assigned on its own line so errexit sees a failed jwk; inside
# printf's arguments its status would be lost.
k1="$(jwk agmen-dev-1)"
k2="$(jwk agmen-dev-2)"
jwks_tmp="$(mktemp "$dir/jwks/.jwks.XXXXXX")"
printf '{"keys":[%s,%s]}\n' "$k1" "$k2" >"$jwks_tmp"
chmod 644 "$jwks_tmp"
mv "$jwks_tmp" "$dir/jwks/jwks.json"

# Self-signed TLS for the haproxy service (WSS on 8443 in front of EMQX's
# plain WebSocket listener). Tests trust tls/ca.pem. haproxy/server.pem is
# the certificate followed by its key, as HAProxy wants it; it sits in its
# own directory so compose mounts a directory, never a missing file.
mkdir -p "$dir/tls" "$dir/haproxy"
if [[ ! -f "$dir/tls/ca.pem" ]]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$dir/tls/ca.key" -out "$dir/tls/ca.pem" -days 3650 \
    -subj "/CN=agmen dev CA" 2>/dev/null
  rm -f "$dir/haproxy/server.pem"
fi
if [[ ! -f "$dir/haproxy/server.pem" ]]; then
  ext="$(mktemp)"
  csr="$(mktemp)"
  printf '%s\n' 'basicConstraints=critical,CA:FALSE' \
    'keyUsage=critical,digitalSignature' 'extendedKeyUsage=serverAuth' \
    'subjectAltName=DNS:localhost,IP:127.0.0.1' >"$ext"
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$dir/tls/server.key" -out "$csr" -subj "/CN=localhost" 2>/dev/null
  openssl x509 -req -in "$csr" -CA "$dir/tls/ca.pem" -CAkey "$dir/tls/ca.key" \
    -CAcreateserial -out "$dir/tls/server.crt" -days 825 -extfile "$ext" 2>/dev/null
  cat "$dir/tls/server.crt" "$dir/tls/server.key" >"$dir/haproxy/server.pem"
  rm -f "$ext" "$csr" "$dir/tls/server.crt" "$dir/tls/server.key" "$dir/tls/ca.srl"
fi
chmod 600 "$dir/tls/ca.key"

ensure_env AGMEN_IT_JWT_DIR "$dir/jwt"
ensure_env AGMEN_IT_MQTT_WSS "wss://localhost:${AGMEN_DEV_WSS_PORT:-8443}/mqtt"
ensure_env AGMEN_IT_WSS_CA "$dir/tls/ca.pem"

# EMQX (uid 1000), nginx and HAProxy run as non-root users in their
# containers and must read the files mounted into them.
chmod 755 "$dir/jwks" "$dir/haproxy"
chmod 644 "$dir/api-keys.txt" "$dir/haproxy/server.pem" "$dir/tls/ca.pem"
chmod 600 "$dir/dev.env"
echo "dev secrets ready in $dir"
