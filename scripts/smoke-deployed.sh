#!/usr/bin/env bash
# No credentials or MQTT publish: test public endpoints and a WSS handshake.
set -euo pipefail

usage() {
  echo 'Usage: smoke-deployed.sh HTTPS_HUB_URL [WSS_MQTT_URL]'
  echo 'WSS_MQTT_URL may also come from AGMEN_MQTT_WSS_URL; include /mqtt.'
  echo 'Requires curl. Uses trusted TLS, direct DNS and 10 second request deadlines.'
}
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
if [[ ${1:-} == -h || ${1:-} == --help ]]; then
  usage
  exit 0
fi
[[ $# -ge 1 && $# -le 2 ]] || {
  usage >&2
  exit 1
}
hub=${1%/}
mqtt=${2:-${AGMEN_MQTT_WSS_URL:-}}
# Accept an HTTPS origin and a WSS endpoint. Refuse userinfo, queries and
# fragments so credentials cannot enter URLs or diagnostic messages.
[[ $hub =~ ^https://[a-zA-Z0-9.-]+(:[0-9]{1,5})?$ ]] || fail 'hub URL must be an HTTPS origin without credentials, a path, query or fragment'
[[ $mqtt =~ ^wss://[a-zA-Z0-9.-]+(:[0-9]{1,5})?/mqtt$ ]] || fail 'provide a WSS MQTT URL ending in /mqtt (see --help)'
command -v curl >/dev/null || fail 'missing dependency: curl'

for endpoint in healthz v1/jwks v1/me; do
  expected=200
  [[ $endpoint != v1/me ]] || expected=401
  status=$(curl -q --silent --noproxy '*' --proto '=https' --connect-timeout 5 --max-time 10 --output /dev/null --write-out '%{http_code}' "$hub/$endpoint") || fail "GET /$endpoint failed (DNS, TLS, connection or timeout)"
  [[ $status == "$expected" ]] || fail "GET /$endpoint returned HTTP $status, expected $expected"
  echo "OK: GET /$endpoint returned HTTP $status"
done

scratch=$(mktemp -d) || fail 'cannot create temporary directory'
trap 'rm -rf "$scratch"' EXIT
# The RFC 6455 example nonce is public. Its expected accept value below
# verifies that the response is a WebSocket handshake, not just a 101 page.
result=0
status=$(curl -q --silent --noproxy '*' --proto '=https' --http1.1 \
  --connect-timeout 5 --max-time 10 --output /dev/null \
  --dump-header "$scratch/headers" --write-out '%{http_code}' \
  --header 'Connection: Upgrade' --header 'Upgrade: websocket' \
  --header 'Sec-WebSocket-Version: 13' \
  --header 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
  --header 'Sec-WebSocket-Protocol: mqtt' "https://${mqtt#wss://}") || result=$?
# An upgraded connection stays open. Curl's deadline (28) is expected only
# after a complete, valid handshake; other transport errors still fail.
[[ $result == 0 || $result == 28 ]] || fail "MQTT WSS request failed (curl exit $result)"
[[ $status == 101 ]] || fail "MQTT WSS upgrade returned HTTP $status, expected 101"
tr -d '\r' <"$scratch/headers" >"$scratch/normalized"
grep -Eiq '^Upgrade:[[:space:]]*websocket[[:space:]]*$' "$scratch/normalized" || fail 'MQTT WSS response lacks Upgrade: websocket'
grep -Eiq '^Connection:([[:space:]]*[^,]+,)*[[:space:]]*Upgrade([[:space:]]*,.*)?[[:space:]]*$' "$scratch/normalized" || fail 'MQTT WSS response lacks Connection: Upgrade'
awk 'tolower($1) == "sec-websocket-accept:" && $2 == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" && NF == 2 { found = 1 } END { exit !found }' "$scratch/normalized" || fail 'MQTT WSS response has an invalid Sec-WebSocket-Accept'
grep -Eiq '^Sec-WebSocket-Protocol:[[:space:]]*mqtt[[:space:]]*$' "$scratch/normalized" || fail 'MQTT WSS response did not select the mqtt subprotocol'
echo 'OK: MQTT WSS TLS and WebSocket upgrade succeeded'
echo 'Deployed smoke checks passed.'
