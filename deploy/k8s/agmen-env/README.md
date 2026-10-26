# agmen-env: the agmen hub, broker and database

agmen runs in its own Kubernetes cluster with its own Vault, registry, ArgoCD and domain. Nothing here points at GroupDrive infrastructure. The older plans name GroupDrive hosts, the `groupdrive/Kubernetes` repository and its Vault; those are out of date.

Everything that names agmen's own infrastructure is a placeholder until that infrastructure exists:

| Placeholder | Where | Replace with |
|---|---|---|
| `api.agmen.example`, `mqtt-ws.agmen.example` | `ingress.yaml`, `AGMEN_PUBLIC_URL` in `hub.yaml` | The domain Andy picks |
| `registry.agmen.example` | `hub.yaml` image, `REGISTRY` in `Jenkinsfile` | agmen's registry |
| `https://vault.agmen.example`, auth mount `kubernetes-agmen`, role `agmen` | `vso-secrets.yaml` | agmen's Vault (or OpenBao) and its Kubernetes auth mount |
| `https://git.agmen.example/agmen/agmen.git` | `argocd-application.yaml` | Where ArgoCD reads this repository |
| Image tag `build-0` | `hub.yaml` | A real `build-<N>` from the Jenkins `Image` stage |
| No `storageClassName` | PVC templates in `emqx.yaml` and `postgres.yaml` | agmen's Ceph RBD class; unset, the claims take the cluster default |

## What runs

All in namespace `agmen`:

- **`agmen-emqx`**: StatefulSet, one replica, `emqx/emqx:6.3.2` (ADR-0001). One node and no clustering keeps the BSL single-node community terms. `emqx.conf` comes from the `agmen-emqx-conf` ConfigMap, mounted read-only, and holds both authenticators (cinfo, then JWT) because EMQX lets anonymous clients in when it has none (ADR-0001, ADR-0002). The JWT authenticator reads the hub's JWKS at `http://agmen-hub.agmen.svc.cluster.local:8080/v1/jwks`. Authorization is `no_match = deny` with `sources = []`, and `max_packet_size = 96KB`. Data lives on a 5 GiB PVC.
- **`agmen-postgres`**: StatefulSet, `postgres:16.15`, 10 GiB PVC.
- **`agmen-hub`**: Deployment, one replica, `Recreate` (two pods would share the broker client id `agmen-hub-main`). Readiness is `GET /healthz` (the database answers `SELECT 1` within 0.8 s), liveness `GET /livez`. The hub serves HTTP before it connects to EMQX and retries its first broker connect for about two minutes, so a cold start, when EMQX has not yet fetched the JWKS, settles without a crash loop.
- **Ingress** (HAProxy, `ingressClassName: haproxy`, cert-manager `letsencrypt-prod`): `api.` goes to the hub on 8080; `mqtt-ws.` path `/mqtt` goes to EMQX 8083 with a 1 h tunnel timeout. The dashboard and REST API (18083) and plain MQTT (1883) have no ingress.
- **NetworkPolicies** (`network-policies.yaml`): Postgres 5432 accepts only the hub and the dump pods; EMQX 1883 and 18083 accept only the hub; EMQX 8083 stays open for the ingress controller. Traffic inside the cluster is plaintext (`sslmode=disable`, plain MQTT and HTTP), so these policies are the only guard on it.
- **`agmen-pg-dump`**: CronJob at 03:00 `America/Denver` (`.spec.timeZone` set explicitly). An init container runs `pg_dump --format=custom` with `postgres:16.15`, then `amazon/aws-cli:2.38.5` uploads it to `s3://<bucket>/postgres/agmen-<UTC time>.dump` on agmen's Ceph RGW.

## Cluster requirements

The new cluster needs the HAProxy ingress controller (haproxytech, which reads the `haproxy.org/*` annotations), cert-manager with a `letsencrypt-prod` ClusterIssuer, the Vault Secrets Operator, ArgoCD, a Ceph RBD storage class, a network plugin that enforces NetworkPolicy (Calico or Cilium, for example; flannel alone ignores them) and Kubernetes 1.27 or later (CronJob `timeZone`).

## Vault

KV v2 mount `secret`. The VaultStaticSecrets in `vso-secrets.yaml` turn these into Kubernetes secrets and restart what uses them when they change.

| Path | Keys | Used by |
|---|---|---|
| `agmen/postgres` | `username`, `password` | Postgres (`POSTGRES_*`), the hub (`AGMEN_DATABASE_URL`), the dump |
| `agmen/github` | `client_id`, `client_secret` | The hub's GitHub OAuth app |
| `agmen/emqx` | `api_key`, `api_secret`, `dashboard_password`, `node_cookie` | EMQX (API key bootstrap file, dashboard admin, Erlang cookie) and the hub (`AGMEN_EMQX_API_KEY`, `AGMEN_EMQX_API_SECRET`) |
| `agmen/jwt` | `agmen-1.pem` (ES256 PKCS#8 private key) | The hub's broker token signing key; the key id is the name without `.pem` |
| `agmen/backup-s3` | `access_key_id`, `secret_access_key`, `endpoint`, `bucket` | The pg_dump upload |

- The database password goes into a URL unescaped: use letters and digits only (for example `openssl rand -hex 24`).
- Postgres reads `POSTGRES_PASSWORD` only when it creates the data directory. To change the password later, run `ALTER ROLE` in the database first, then update Vault.
- Make a key with `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256`. To rotate: add `agmen-2.pem` to Vault, add `/keys/agmen-2.pem` to `AGMEN_JWT_EXTRA_KEYS` in `hub.yaml`, wait at least 60 s (EMQX refreshes the JWKS every 60 s), then swap the two variables. Drop the old key once its last token has expired (1 h).

## First deploy

1. Create the Vault entries above and a Kubernetes auth role `agmen` bound to service account `agmen-vso` in namespace `agmen`.
2. Replace the placeholders above.
3. Create the pull secret `regcred` in namespace `agmen` for agmen's registry. Keep it out of git.
4. `kubectl apply -f deploy/k8s/agmen-env/argocd-application.yaml`. ArgoCD then syncs the rest of this directory and creates the namespace.
5. Point the two hosts' DNS at the cluster's ingress.
6. Make the first project and admin: `kubectl -n agmen exec deploy/agmen-hub -- agmen-hub bootstrap --tenant <name> --project <name> --admin-github <login>`.

Checks after deploy: `curl -fsS https://api.agmen.example/healthz` prints `ok`, and `curl -fsS https://api.agmen.example/v1/jwks` lists the key ids.

## Backups

Set a lifecycle rule on the bucket to expire old dumps; the CronJob never deletes any. Restore with `pg_restore --clean --if-exists -d agmen agmen-<time>.dump` against a stopped hub.

## Checked

- `kubeconform` v0.8.0 (from its container image, `-strict`, with the CRDs-catalog schemas for VSO and ArgoCD): 25 resources, all valid.
- `kubectl apply --dry-run=client` needs a cluster to resolve kinds, and none was configured when these were written.
- Not checked against a live cluster: everything above, including the `haproxy.org/timeout-tunnel` annotation and VSO's handling of `excludes` with templates.
