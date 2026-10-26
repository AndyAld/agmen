# agmen-env: the agmen hub, broker and database

agmen runs in its own Kubernetes cluster, with its own secret store, registry and domain. Nothing here points at GroupDrive infrastructure. The older plans name GroupDrive hosts, the `groupdrive/Kubernetes` repository and its Vault; those are out of date.

This directory is a kustomization. The default set (`kustomization.yaml`) is everything agmen runs, with no secret store and no settings for one ingress controller. The pods read Kubernetes Secrets by name (see "Secrets"), so any secret store can fill them. `examples/` holds optional pieces for one setup: Vault through the Vault Secrets Operator, the HAProxy ingress controller, and ArgoCD.

Build it with `kubectl kustomize deploy/k8s/agmen-env` and apply it with `kubectl apply -k deploy/k8s/agmen-env`. `kubectl apply -f` on the directory does not work, since it does not read `kustomization.yaml`.

## Placeholders

Everything that names agmen's own infrastructure is a placeholder until that infrastructure exists. Most are in `placeholders.yaml`, which `kustomization.yaml` copies into the manifests and which never reaches the cluster.

| Placeholder | Where | Sets | Replace with |
|---|---|---|---|
| `ingress-class` (`placeholder-ingress-class`) | `placeholders.yaml` | `ingressClassName` of both Ingresses | The IngressClass of the cluster's ingress controller |
| `cluster-issuer` (`placeholder-cluster-issuer`) | `placeholders.yaml` | `cert-manager.io/cluster-issuer` on both Ingresses | The cert-manager ClusterIssuer for the two hosts |
| `api-host` (`api.agmen.example`) | `placeholders.yaml` | The `agmen-api` Ingress host and TLS host, and the host in the hub's `AGMEN_PUBLIC_URL` | The API host on the domain Andy picks |
| `mqtt-ws-host` (`mqtt-ws.agmen.example`) | `placeholders.yaml` | The `agmen-mqtt-ws` Ingress host and TLS host, and the host in the hub's `AGMEN_PUBLIC_MQTT_URL` (`wss://<host>/mqtt`, the broker URL `GET /v1/me` gives clients) | The MQTT over WebSocket host |
| `hub-image` (`registry.agmen.example/agmen-hub:build-0`) | `placeholders.yaml` | The hub container image | agmen's registry and a real `build-<N>` from the Jenkins `Image` stage (`build-0` is a tag no build produces) |
| `AGMEN_REGISTRY` (unset) | Jenkins global environment variable (Manage Jenkins, System) | The registry the `Image` stage pushes `agmen-hub:build-<N>` to, logging in with the Jenkins credential `agmen-registry` (username and password). While it is unset, the stage is skipped. | agmen's registry host, the same as in `hub-image` |
| No `storageClassName` | PVC templates in `emqx.yaml` and `postgres.yaml` | The volumes' storage class; unset, the claims take the cluster default | agmen's Ceph RBD class, if it is not the default |
| Ingress controller annotations (none) | A component of your own, like `examples/haproxy-ingress` | Controller settings: HTTPS redirect, and an idle timeout over 30 s for the MQTT WebSocket (clients send a keep-alive every 30 s, ADR-0003) | Your controller's annotations |
| Secret store (none) | A component of your own, like `examples/vso` | The Kubernetes Secrets in "Secrets" | Your secret store's resources |
| `https://vault.agmen.example`, auth mount `kubernetes-agmen`, role `agmen` | `examples/vso/vso-secrets.yaml` | Only with the VSO example | agmen's Vault (or OpenBao) and its Kubernetes auth mount |
| `https://git.agmen.example/agmen/agmen.git` | `examples/argocd/application.yaml` | Only with the ArgoCD example | Where ArgoCD reads this repository |

## What runs

All in namespace `agmen`:

- **`agmen-emqx`**: StatefulSet, one replica, `emqx/emqx:6.3.2` (ADR-0001). One node and no clustering keeps the BSL single-node community terms. Its config is `emqx.conf` in this directory, mounted read-only from a ConfigMap, and holds both authenticators (cinfo, then JWT) because EMQX lets anonymous clients in when it has none (ADR-0001, ADR-0002). The JWT authenticator reads the hub's JWKS at `http://agmen-hub.agmen.svc.cluster.local:8080/v1/jwks`. Authorization is `no_match = deny` with `sources = []`, and `max_packet_size = 96KB`. Data lives on a 5 GiB PVC.
- **`agmen-postgres`**: StatefulSet, `postgres:16.15`, 10 GiB PVC.
- **`agmen-hub`**: Deployment, one replica, `Recreate` (two pods would share the broker client id `agmen-hub-main`). Readiness is `GET /healthz` (the database answers `SELECT 1` within 0.8 s), liveness `GET /livez`. The hub serves HTTP before it connects to EMQX and retries its first broker connect for about two minutes, so a cold start, when EMQX has not yet fetched the JWKS, settles without a crash loop.
- **Ingress**: `api.` goes to the hub on 8080; `mqtt-ws.` path `/mqtt` goes to EMQX 8083. Each host gets a cert-manager certificate. The dashboard and REST API (18083) and plain MQTT (1883) have no ingress.
- **NetworkPolicies** (`network-policies.yaml`): Postgres 5432 accepts only the hub and the dump pods; EMQX 1883 and 18083 accept only the hub; EMQX 8083 stays open for the ingress controller. Traffic inside the cluster is plaintext (`sslmode=disable`, plain MQTT and HTTP), so these policies are the only guard on it.
- **`agmen-pg-dump`**: CronJob at 03:00 `America/Denver` (`.spec.timeZone` set explicitly). An init container runs `pg_dump --format=custom` with `postgres:16.15`, then `amazon/aws-cli:2.38.5` uploads it to `s3://<bucket>/postgres/agmen-<UTC time>.dump` on agmen's S3 compatible store (Ceph RGW).

### Changing the EMQX config

`kustomization.yaml` makes the `agmen-emqx-conf` ConfigMap from `emqx.conf` with a hash of the content in its name, so a changed `emqx.conf` changes the StatefulSet's pod template and the pod restarts with it. The file is mounted with `subPath`, which never sees an update to a ConfigMap in place, so a change applied any other way needs `kubectl -n agmen rollout restart statefulset/agmen-emqx`. Check the authenticators after any change: with none, EMQX lets anonymous clients in.

## Secrets

The pods read these Kubernetes Secrets in namespace `agmen`. They are the interface: create them with any secret store, or by hand with `kubectl create secret`, but keep their values out of git. A pod reads a secret when it starts, so whatever fills them must restart the workloads in the last column after a change.

| Secret | Key | Read by | Value |
|---|---|---|---|
| `agmen-postgres` | `POSTGRES_USER` | `agmen-postgres` (all keys as env), `agmen-pg-dump` (as `PGUSER`) | The database superuser |
| | `POSTGRES_PASSWORD` | `agmen-postgres`, `agmen-pg-dump` (as `PGPASSWORD`) | Its password |
| | `POSTGRES_DB` | `agmen-postgres` | `agmen`; the hub's URL and the dump use that name |
| `agmen-hub-database` | `AGMEN_DATABASE_URL` | `agmen-hub` (all keys as env) | `postgres://<user>:<password>@agmen-postgres:5432/agmen?sslmode=disable`, with the same user and password as `agmen-postgres` |
| `agmen-hub-github` | `AGMEN_GITHUB_CLIENT_ID`, `AGMEN_GITHUB_CLIENT_SECRET` | `agmen-hub` (all keys as env) | The hub's GitHub OAuth app |
| `agmen-hub-emqx-api` | `AGMEN_EMQX_API_KEY`, `AGMEN_EMQX_API_SECRET` | `agmen-hub` (all keys as env) | The EMQX REST API key and secret the hub uses |
| `agmen-emqx` | `api-keys.txt` | `agmen-emqx`, mounted at `/opt/emqx/etc/agmen/api-keys.txt` | One line, `<key>:<secret>:administrator`, with the same key and secret as `agmen-hub-emqx-api` |
| | `EMQX_DASHBOARD__DEFAULT_PASSWORD` | `agmen-emqx` | The dashboard admin password, instead of the image's public default |
| | `EMQX_NODE__COOKIE` | `agmen-emqx` | The Erlang node cookie, instead of the image's public default |
| `agmen-jwt-keys` | `agmen-1.pem` (one key per file) | `agmen-hub`, mounted at `/keys` | An ES256 PKCS#8 PEM private key; the key id is the name without `.pem` |
| `agmen-backup-s3` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `S3_ENDPOINT`, `S3_BUCKET` | `agmen-pg-dump` upload (all keys as env) | The backup bucket and its credentials |
| `regcred` | `.dockerconfigjson` (type `kubernetes.io/dockerconfigjson`) | `agmen-hub` image pull | A pull secret for agmen's registry |

Restart after a change: `agmen-postgres` restarts the `agmen-postgres` StatefulSet; `agmen-hub-database`, `agmen-hub-github`, `agmen-hub-emqx-api` and `agmen-jwt-keys` restart the `agmen-hub` Deployment; `agmen-emqx` restarts the `agmen-emqx` StatefulSet. The dump reads `agmen-postgres` and `agmen-backup-s3` fresh each night.

- The secrets marked "all keys as env" are read with `envFrom`, which passes every key in them to the process, so keep only the keys above in them.
- The database password goes into a URL unescaped: use letters and digits only (for example `openssl rand -hex 24`).
- Postgres reads `POSTGRES_PASSWORD` only when it creates the data directory. To change the password later, run `ALTER ROLE` in the database first, then update both secrets.
- Make a key with `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256`. To rotate: add `agmen-2.pem` to `agmen-jwt-keys`, add `/keys/agmen-2.pem` to `AGMEN_JWT_EXTRA_KEYS` in `hub.yaml`, wait at least 60 s (EMQX refreshes the JWKS every 60 s), then swap the two variables. Drop the old key once its last token has expired (1 h).

### Example filler: Vault through the Vault Secrets Operator

`examples/vso` is a kustomize component that fills every secret above except `regcred` from Vault or OpenBao (KV v2, mount `secret`), and restarts the workloads above when a value changes. It needs the VSO CRDs and a Kubernetes auth role `agmen` bound to service account `agmen-vso` in namespace `agmen`.

| Vault path | Keys | Fills |
|---|---|---|
| `agmen/postgres` | `username`, `password` | `agmen-postgres`, `agmen-hub-database` |
| `agmen/github` | `client_id`, `client_secret` | `agmen-hub-github` |
| `agmen/emqx` | `api_key`, `api_secret`, `dashboard_password`, `node_cookie` | `agmen-hub-emqx-api`, `agmen-emqx` |
| `agmen/jwt` | `agmen-1.pem` | `agmen-jwt-keys` (every key, as is) |
| `agmen/backup-s3` | `access_key_id`, `secret_access_key`, `endpoint`, `bucket` | `agmen-backup-s3` |

To use it, list it under `components:` in a kustomization that includes this directory, or in the ArgoCD Application (`examples/argocd/application.yaml` does). Not tested against a live VSO: its handling of `excludes` together with templates.

## Cluster requirements

An ingress controller, cert-manager with a ClusterIssuer, a storage class for the two PVCs, a network plugin that enforces NetworkPolicy (Calico or Cilium, for example; flannel alone ignores them) and Kubernetes 1.27 or later (CronJob `timeZone`). Whatever fills the secrets, and whatever applies the kustomization (`kubectl apply -k`, or a GitOps tool such as ArgoCD), are the cluster owner's choice.

## First deploy

1. Fill `placeholders.yaml`, and add a component for the ingress controller's annotations if it needs any.
2. Create the secrets above in namespace `agmen` (create the namespace first, or let the first apply create it and the pods wait for the secrets).
3. Apply: `kubectl apply -k deploy/k8s/agmen-env`, or a kustomization of your own that includes this directory and your components. With ArgoCD, fill and apply `examples/argocd/application.yaml` once; ArgoCD then syncs the rest.
4. Point the two hosts' DNS at the cluster's ingress.
5. Make the first project and admin: `kubectl -n agmen exec deploy/agmen-hub -- agmen-hub bootstrap --tenant <name> --project <name> --admin-github <login>`.

Checks after deploy: `curl -fsS https://<api-host>/healthz` prints `ok`, and `curl -fsS https://<api-host>/v1/jwks` lists the key ids.

## Backups

Set a lifecycle rule on the bucket to expire old dumps; the CronJob never deletes any. Restore with `pg_restore --clean --if-exists -d agmen agmen-<time>.dump` against a stopped hub.

## Checked

- `kubectl kustomize` (kustomize v5.8.1) builds the default set; `kubeconform` v0.8.0 (from its container image, `-strict`, core schemas only) finds 13 resources, all valid.
- The default set with `examples/vso` and `examples/haproxy-ingress` as components: 23 resources, all valid with the CRDs-catalog schemas for VSO. `examples/argocd/application.yaml`: 2 resources, valid with the ArgoCD schemas.
- Changing every value in `placeholders.yaml` changes exactly the fields in the table above, and changing `emqx.conf` changes the ConfigMap name in the EMQX pod template.
- `kubectl apply --dry-run=client -k` needs a cluster to resolve kinds, and none was configured when these were written.
- Not checked against a live cluster: everything above, including HAProxy's `haproxy.org/timeout-tunnel` annotation.
