#!/usr/bin/env bash
# Read-only checks against the current kubeconfig context.
set -euo pipefail

usage() {
  echo 'Usage: cluster-check.sh [--api-host HOST] [--mqtt-host HOST]'
  echo 'Required hosts may also come from AGMEN_API_HOST and AGMEN_MQTT_WS_HOST.'
  echo 'Requires kubectl, curl and GNU timeout (timeout or gtimeout).'
}
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

api_host=${AGMEN_API_HOST:-}
mqtt_host=${AGMEN_MQTT_WS_HOST:-}
while (($#)); do
  case "$1" in
  --api-host | --mqtt-host)
    (($# >= 2)) || {
      usage >&2
      exit 1
    }
    if [[ $1 == --api-host ]]; then api_host=$2; else mqtt_host=$2; fi
    shift 2
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 1
    ;;
  esac
done
for host in "$api_host" "$mqtt_host"; do
  [[ $host =~ ^[a-zA-Z0-9]([a-zA-Z0-9.-]*[a-zA-Z0-9])?$ ]] || fail 'provide both hosts as DNS names, without schemes, paths or ports (see --help)'
done
for tool in kubectl curl; do
  command -v "$tool" >/dev/null || fail "missing dependency: $tool"
done
if command -v timeout >/dev/null; then
  deadline=timeout
elif command -v gtimeout >/dev/null; then
  deadline=gtimeout
else
  fail 'missing dependency: GNU timeout (timeout or gtimeout)'
fi

# The outer deadline also bounds exec's streaming connection. Suppress server
# errors so a failing Secret request cannot put response data in the output.
kube() { "$deadline" --kill-after=2s 15s kubectl --request-timeout=10s -n agmen "$@" 2>/dev/null; }
kube get namespace agmen -o name >/dev/null || fail 'namespace agmen is missing or inaccessible'
echo 'OK: namespace agmen exists'

for name in agmen-emqx agmen-postgres; do
  state=$(kube get statefulset "$name" -o 'jsonpath={.metadata.generation}|{.status.observedGeneration}|{.spec.replicas}|{.status.readyReplicas}|{.status.updatedReplicas}|{.status.currentRevision}|{.status.updateRevision}') || fail "cannot read StatefulSet $name"
  IFS='|' read -r generation observed desired ready updated current revision <<<"$state"
  [[ $desired =~ ^[1-9][0-9]*$ && $generation == "$observed" && $ready == "$desired" && $updated == "$desired" && -n $current && $current == "$revision" ]] || fail "StatefulSet $name is not fully ready at its current revision"
  echo "OK: StatefulSet $name is ready"
done
state=$(kube get deployment agmen-hub -o 'jsonpath={.metadata.generation}|{.status.observedGeneration}|{.spec.replicas}|{.status.readyReplicas}|{.status.availableReplicas}|{.status.conditions[?(@.type=="Available")].status}') || fail 'cannot read Deployment agmen-hub'
IFS='|' read -r generation observed desired ready available condition <<<"$state"
[[ $desired =~ ^[1-9][0-9]*$ && $generation == "$observed" && $ready == "$desired" && $available == "$desired" && $condition == True ]] || fail 'Deployment agmen-hub is not available'
echo 'OK: Deployment agmen-hub is available'

# JSONPath tests key presence and returns only the Secret name, never values.
for secret in agmen-hub-database agmen-hub-github agmen-hub-emqx-api agmen-jwt-keys regcred; do
  case "$secret" in
  agmen-hub-database) keys=(AGMEN_DATABASE_URL) ;;
  agmen-hub-github) keys=(AGMEN_GITHUB_CLIENT_ID AGMEN_GITHUB_CLIENT_SECRET) ;;
  agmen-hub-emqx-api) keys=(AGMEN_EMQX_API_KEY AGMEN_EMQX_API_SECRET) ;;
  agmen-jwt-keys) keys=('agmen-1.pem') ;;
  regcred) keys=('.dockerconfigjson') ;;
  esac
  for key in "${keys[@]}"; do
    field=${key//./\\.}
    found=$(kube get secret --field-selector "metadata.name=$secret" -o "jsonpath={.items[?(@.data.$field)].metadata.name}") || fail "cannot check Secret $secret key $key"
    [[ $found == "$secret" ]] || fail "Secret $secret is missing key $key or does not exist"
  done
  echo "OK: Secret $secret has the required key names"
done

config=$(kube exec --pod-running-timeout=10s statefulset/agmen-emqx -c emqx -- /opt/emqx/bin/emqx ctl conf show durable_sessions) || fail 'cannot read effective EMQX durable_sessions config (requires pods/exec permission)'
setting=$(printf '%s\n' "$config" | sed -n '/^[[:space:]]*enable[[:space:]]*=/p')
[[ $setting =~ ^[[:space:]]*enable[[:space:]]*=[[:space:]]*false[[:space:]]*$ ]] || fail 'EMQX durable_sessions.enable must be false (R71)'
echo 'OK: EMQX durable_sessions.enable is false (R71)'

for ingress in agmen-api agmen-mqtt-ws; do
  if [[ $ingress == agmen-api ]]; then
    host=$api_host
    path=/healthz
  else
    host=$mqtt_host
    path=/mqtt
  fi
  hosts=$(kube get ingress "$ingress" -o 'jsonpath={.spec.rules[*].host}|{.spec.tls[*].hosts[*]}') || fail "cannot read Ingress $ingress"
  [[ $hosts == "$host|$host" ]] || fail "Ingress $ingress rule and TLS hosts must match the supplied host"
  status=$(curl -q --silent --noproxy '*' --proto '=https' --connect-timeout 5 --max-time 10 --output /dev/null --write-out '%{http_code}' "https://$host$path") || fail "Ingress $ingress DNS, TLS or HTTPS request failed"
  # A plain GET to EMQX's upgrade endpoint may return 400. Any HTTP response
  # proves HTTPS reachability; smoke-deployed.sh checks the actual upgrade.
  [[ $status =~ ^[1-5][0-9][0-9]$ ]] || fail "Ingress $ingress did not answer HTTPS"
  if [[ $ingress == agmen-api && $status != 200 ]]; then fail "Ingress $ingress /healthz returned HTTP $status, expected 200"; fi
  echo "OK: Ingress $ingress resolves and answers HTTPS (HTTP $status)"
done
echo 'Cluster checks passed.'
