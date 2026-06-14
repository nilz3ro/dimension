# Dimension Deployment Guide

> Dimension is a serverless compute launcher for Firecracker microVMs.
> POST /run boots a VM, delivers the JSON payload to the bundle's stdin over vsock
> (via the `dimension-agent` sidecar), and streams run events out as SSE
> (`GET /runs/{id}/events`). Sync mode returns stdout inline; async returns an
> invocation ID + events URL.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Binaries](#binaries)
3. [Infrastructure Services](#infrastructure-services)
4. [Environment Variables](#environment-variables)
5. [Database](#database)
6. [Gateway Deployment](#gateway-deployment)
7. [Worker Deployment](#worker-deployment)
8. [CLI Setup](#cli-setup)
9. [Bundle Workflow](#bundle-workflow)
10. [Telegram Bridge Deployment](#telegram-bridge-deployment)
11. [Verification Checklist](#verification-checklist)
12. [API Reference](#api-reference)
13. [Observability](#observability)
14. [Troubleshooting](#troubleshooting)
15. [What Changed in M003](#what-changed-in-m003)

---

## Architecture Overview

```
  Developer                           Gateway                         Worker(s)
  ────────                           ───────                         ─────────
                                                                     ┌─────────────────┐
  dimension build                    dimension-server (:3000)        │ dimension-worker │
  dimension push ──────────────────► POST /bundles/push ────────────►│ PushBundle gRPC  │
  dimension run  ──────────────────► POST /run ─────────────────────►│ RunInvocation    │
  dimension stop ──────────────────► POST /run/{id}/stop ──────────►│ StopInvocation   │
  dimension logs ──────────────────► GET /invocations/{id} ◄────────│                  │
  SSE: GET /runs/{id}/events ◄────── (replay: Clickhouse,            │   Firecracker    │
                                      live: Pulsar)                  │   ┌──────────┐   │
                                         │                           │   │ hyphae-  │   │
                                     Postgres                        │   │ init     │   │
                                     Clickhouse ◄────────────────────│   │ (PID 1)  │   │
                                     Pulsar     ◄────────────────────│   │  ↓       │   │
                                     MinIO ◄─────────────────────────│   │ dimension-   │
                                                                     │   │ agent ↔ vsock│
                                                                     │   │  ↓ stdin     │
                                                                     │   │ entry-   │   │
                                                                     │   │ point    │   │
                                                                     │   └──────────┘   │
                                                                     └─────────────────┘
```

**Key concepts:**

- **Gateway** (`dimension-server`): HTTP API server. Routes invocations to workers via gRPC. Stores bundle metadata in Postgres. Serves run events as SSE (replaying history from Clickhouse, tailing live events from Pulsar). Proxies Clickhouse/MinIO queries for `dimension logs`.
- **Worker** (`dimension-worker`): gRPC server. Runs Firecracker VMs. Receives bundles via `PushBundle` RPC. Fans guest run events out to Clickhouse + Pulsar. Uploads logs to MinIO.
- **CLI** (`dimension-cli`): Developer tool. `build` → `push` → `run` → `logs`. Stores credentials in `~/.dimension/credentials`.
- **dimension-agent**: in-VM sidecar started by hyphae-init. Receives the JSON payload over vsock and pipes it to the bundle's stdin; bridges the bundle's outbound events (`DIMENSION_EVENTS_SOCK` Unix socket) and stdout/stderr back to the worker.
- **Three invocation modes**:
  - **sync**: Blocks, returns stdout as response body (via vsock)
  - **async**: Returns 202 + invocation_id, VM runs to completion
  - **persistent**: Returns 202 + invocation_id, VM stays up until `dimension stop` or process exit

---

## Binaries

| Binary | Crate | Purpose | Where to Deploy |
|---|---|---|---|
| `dimension-server` | `crates/dimension-gateway` | HTTP API, bundle push/rollback, invocation dispatch, admin endpoints | Gateway node |
| `dimension-worker` | `crates/dimension-worker` | gRPC server, Firecracker VM execution, Clickhouse/MinIO observability | Worker node(s) with KVM |
| `dimension-cli` | `crates/dimension-cli` | Developer CLI (build, push, run, stop, logs, login, rollback) | Developer machines |

### Building

```bash
# All three binaries
cargo build --release -p dimension-gateway -p dimension-worker -p dimension-cli

# Output locations
target/release/dimension-server   # gateway
target/release/dimension-worker   # worker
target/release/dimension-cli      # CLI (copy to developer machines)
```

**Build requirements:**
- Rust stable (2024 edition)
- `protoc` (protobuf compiler) — required by `tonic-build` for gRPC code generation
- Linux x86_64 (Firecracker is Linux-only)

**Note:** The build script compiles `dimension-agent` (the in-VM sidecar) and embeds it into rootfs builds. On non-Linux hosts it falls back to a placeholder with a warning; the real binary is built via Docker during `dimension build`.

---

## Infrastructure Services

### Required

| Service | Purpose | Default Port | Notes |
|---|---|---|---|
| **PostgreSQL 16** | User, session, bundle, API key metadata | 5432 | Migrations run automatically on gateway startup |

### Required for Observability

| Service | Purpose | Default Port | Notes |
|---|---|---|---|
| **Clickhouse** | Invocation metadata (append-only index) | 8123 (HTTP) | Worker inserts. Gateway queries. Self-hosted container. |
| **MinIO** (or S3-compatible) | Invocation log storage (stdout/stderr files) | 9000 | Worker uploads. Gateway fetches. Bucket: `dimension-logs`. |

### Optional (Graceful Degradation)

| Service | Purpose | Default Port | When Absent |
|---|---|---|---|
| **Garage / MinIO** (bundle storage) | S3-compatible object storage for bundle artifacts | 3900 | Bundle artifact storage API disabled |
| **HashiCorp Vault** | Secret management, per-VM token minting | 8200 | Secrets disabled, VMs get no DIMENSION_VM_TOKEN |

### Required for Run Event Streaming

| Service | Purpose | Default Port | When Absent |
|---|---|---|---|
| **Apache Pulsar** | Live run-event fan-out to the gateway's SSE streams | 6650 | SSE serves Clickhouse history only — live tail degraded |

> **History note:** M003 briefly removed Pulsar, the in-VM agent, and event
> streaming in favor of MMDS + raw stdout. The outbound message center
> (UDS → vsock → Pulsar/Clickhouse → SSE) reinstated `dimension-agent` and
> Pulsar; payloads are now delivered over vsock, not MMDS.

### Docker Compose (Gateway Node)

Run these via Docker Compose on the gateway node. An example compose file:

```yaml
services:
  postgres:
    image: postgres:16-alpine
    restart: unless-stopped
    environment:
      POSTGRES_DB: dimension
      POSTGRES_USER: dimension
      POSTGRES_PASSWORD: "${DB_PASSWORD}"
    volumes:
      - postgres_data:/var/lib/postgresql/data
    ports:
      - "127.0.0.1:5432:5432"

  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    restart: unless-stopped
    volumes:
      - clickhouse_data:/var/lib/clickhouse
    ports:
      - "127.0.0.1:8123:8123"
    ulimits:
      nofile:
        soft: 262144
        hard: 262144

  minio:
    image: minio/minio:latest
    restart: unless-stopped
    command: server /data --console-address ":9001"
    environment:
      MINIO_ROOT_USER: "${MINIO_ACCESS_KEY}"
      MINIO_ROOT_PASSWORD: "${MINIO_SECRET_KEY}"
    volumes:
      - minio_data:/data
    ports:
      - "127.0.0.1:9000:9000"
      - "127.0.0.1:9001:9001"

  # Optional: Vault for secret management
  vault:
    image: hashicorp/vault:1.18
    restart: unless-stopped
    cap_add:
      - IPC_LOCK
    command: vault server -config=/vault/config/vault.hcl
    volumes:
      - vault_data:/vault/data
      - ./vault.hcl:/vault/config/vault.hcl:ro
    ports:
      - "127.0.0.1:8200:8200"

  # Optional: Garage for bundle artifact storage
  garage:
    image: dxflrs/garage:v2.2.0
    restart: unless-stopped
    volumes:
      - garage_data:/var/lib/garage/data
      - garage_meta:/var/lib/garage/meta
      - ./garage.toml:/etc/garage.toml:ro
    ports:
      - "127.0.0.1:3900:3900"

volumes:
  postgres_data:
  clickhouse_data:
  minio_data:
  vault_data:
  garage_data:
  garage_meta:
```

**Important:** Create the MinIO bucket before first use:

```bash
# Using mc (MinIO client)
mc alias set local http://localhost:9000 $MINIO_ACCESS_KEY $MINIO_SECRET_KEY
mc mb local/dimension-logs
```

---

## Environment Variables

### Gateway (`dimension-server`)

#### Required

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_TOKEN` | — | Bearer token for API authentication |
| `DATABASE_URL` | — | PostgreSQL connection string |

#### Network

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_PORT` | `3000` | HTTP listen port |
| `DIMENSION_HOST` | `0.0.0.0` | Bind address |

#### VM Configuration (only needed if running VMs locally in hybrid mode)

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_KERNEL_PATH` | `/opt/hyphae/kernel/vmlinux` | Kernel binary for VM boot |
| `DIMENSION_FIRECRACKER_BIN` | `firecracker` | Firecracker binary path |
| `DIMENSION_REGISTRY_PATH` | `/var/lib/hyphae` | Hyphae bundle registry dir |
| `DIMENSION_ENABLE_NETWORK` | `false` | TAP+NAT for VM internet access |
| `DIMENSION_MOCK` | `false` | Use mock handler (dev/test) |

#### Timeouts & Limits

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_BOOT_TIMEOUT_SECS` | `30` | VM boot timeout |
| `DIMENSION_PROCESSING_TIMEOUT_SECS` | `300` | VM processing timeout |
| `DIMENSION_MAX_BOOT_TIMEOUT_SECS` | `60` | Max boot timeout ceiling |
| `DIMENSION_MAX_PROCESSING_TIMEOUT_SECS` | `600` | Max processing timeout ceiling |
| `DIMENSION_MAX_CONCURRENT` | `200` | Max concurrent VMs |
| `DIMENSION_MAX_VCPUS` | `8` | Max vCPUs per request |
| `DIMENSION_MAX_MEMORY_MIB` | `8192` | Max memory per request (MiB) |
| `DIMENSION_MAX_DISK_SIZE_MIB` | `65536` | Max disk per request (MiB) |
| `DIMENSION_DRAIN_TIMEOUT_SECS` | `60` | Graceful shutdown drain timeout |

#### Multi-Host Workers

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_MULTI_HOST` | `false` | Enable worker dispatch via gRPC |
| `DIMENSION_WORKER_HEALTH_INTERVAL_SECS` | `15` | Worker health poll interval |

#### Invocation Observability (Clickhouse + Log MinIO)

| Env Var | Default | Description |
|---|---|---|
| `CLICKHOUSE_URL` | `http://localhost:8123` | Clickhouse HTTP endpoint (for gateway queries) |
| `CLICKHOUSE_DATABASE` | `dimension` | Clickhouse database name |
| `LOG_MINIO_ENDPOINT` | `http://127.0.0.1:9000` | MinIO endpoint for log retrieval |
| `LOG_MINIO_BUCKET` | `dimension-logs` | MinIO bucket for invocation logs |
| `LOG_MINIO_ACCESS_KEY` | — | MinIO access key (optional — degrades gracefully) |
| `LOG_MINIO_SECRET_KEY` | — | MinIO secret key (optional — degrades gracefully) |

#### Vault (Optional)

| Env Var | Default | Description |
|---|---|---|
| `VAULT_ADDR` | `http://127.0.0.1:8200` | Vault server URL |
| `VAULT_ROLE_ID` | — | AppRole role ID |
| `VAULT_SECRET_ID` | — | AppRole secret ID |
| `VAULT_RENEWAL_INTERVAL_SECS` | `900` | Token renewal interval |
| `VAULT_VM_TOKEN_TTL_SECS` | `3600` | Per-VM token TTL |

#### Bundle Storage / MinIO (Optional)

| Env Var | Default | Description |
|---|---|---|
| `MINIO_ENDPOINT` | `http://127.0.0.1:9000` | S3-compatible endpoint for bundle artifacts |
| `MINIO_BUCKET` | `dimension` | Bucket name |
| `MINIO_ACCESS_KEY` | — | MinIO access key |
| `MINIO_SECRET_KEY` | — | MinIO secret key |

#### Platform Proxy (Optional)

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_PROXY_JWT_SECRET` | — | JWT signing secret (enables platform proxy) |
| `DIMENSION_PROXY_PORT` | `8765` | Proxy listen port |

---

### Worker (`dimension-worker`)

#### Required

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_GATEWAY_URL` | — | Gateway HTTP URL for registration (e.g. `http://gateway:3000`) |

#### Core

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_WORKER_GRPC_ADDR` | `0.0.0.0:50051` | gRPC listen address |
| `DIMENSION_WORKER_ADVERTISE_ADDR` | (same as grpc_addr) | Address the gateway uses to reach this worker |
| `DIMENSION_WORKER_MEMORY_MB` | `4096` | Total memory available for VMs |
| `DIMENSION_WORKER_VCPUS` | `4` | Total vCPUs available for VMs |
| `DIMENSION_WORKER_HEARTBEAT_INTERVAL_SECS` | `15` | Re-registration heartbeat interval |

#### VM Configuration

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_KERNEL_PATH` | `/opt/hyphae/kernel/vmlinux` | Kernel binary path |
| `DIMENSION_FIRECRACKER_BIN` | `firecracker` | Firecracker binary path |
| `DIMENSION_REGISTRY_PATH` | `/var/lib/hyphae` | Local bundle registry dir |
| `DIMENSION_ENABLE_NETWORK` | `false` | TAP+NAT for VM internet access |
| `DIMENSION_BOOT_TIMEOUT_SECS` | `30` | VM boot timeout |
| `DIMENSION_PROCESSING_TIMEOUT_SECS` | `300` | VM processing timeout |

#### Observability (Clickhouse + MinIO)

| Env Var | Default | Description |
|---|---|---|
| `CLICKHOUSE_URL` | `http://localhost:8123` | Clickhouse HTTP endpoint (for worker inserts) |
| `CLICKHOUSE_DATABASE` | `dimension` | Clickhouse database name |
| `LOG_MINIO_ENDPOINT` | — | MinIO endpoint for log uploads (optional) |
| `LOG_MINIO_BUCKET` | `dimension-logs` | MinIO bucket for logs |
| `LOG_MINIO_ACCESS_KEY` | — | MinIO access key (optional) |
| `LOG_MINIO_SECRET_KEY` | — | MinIO secret key (optional) |

#### Optional

| Env Var | Default | Description |
|---|---|---|
| `DATABASE_URL` | — | PostgreSQL connection string (optional — enables platform dispatch stores on worker) |

---

### CLI (`dimension-cli`)

| Env Var | Default | Description |
|---|---|---|
| `DIMENSION_URL` | `http://localhost:3000` | Gateway base URL |
| `DIMENSION_TOKEN` | — | API token (falls back to `~/.dimension/credentials`) |

---

## Database

### PostgreSQL Migrations

Migrations are in `crates/dimension-store/migrations/` and **run automatically** on gateway startup. On first run, a bootstrap admin API key is printed to stderr — **save it**.

**Current migration count:** 18 migrations (`0001` through `0018`).

### Clickhouse Table

The `invocations` table is created automatically by the worker on startup via `ensure_table()`. Schema:

```sql
CREATE TABLE IF NOT EXISTS invocations (
    invocation_id String,
    user_id String,
    bundle_id String,
    worker_id String,
    mode String,
    status String,           -- running, completed, failed, stopped
    exit_code Int32,
    duration_ms Int64,
    log_url String,           -- MinIO path: invocations/{id}/output.log
    created_at DateTime64(3),
    completed_at DateTime64(3)
) ENGINE = MergeTree()
ORDER BY (created_at, invocation_id)
```

Both gateway and worker connect to the same Clickhouse instance. Worker inserts records. Gateway queries them.

---

## Gateway Deployment

### Prerequisites

- PostgreSQL 16+ accessible
- Clickhouse accessible (for `GET /invocations/{id}` queries)
- MinIO accessible (for log retrieval via `?include_logs=true`)
- Network access from workers to gateway port 3000 (for registration)
- Network access from gateway to worker gRPC port 50051

### Start

```bash
export DATABASE_URL="postgres://dimension:password@localhost:5432/dimension"
export DIMENSION_TOKEN="your-admin-bearer-token"
export DIMENSION_MULTI_HOST=true

# Observability
export CLICKHOUSE_URL="http://localhost:8123"
export CLICKHOUSE_DATABASE="dimension"
export LOG_MINIO_ENDPOINT="http://localhost:9000"
export LOG_MINIO_BUCKET="dimension-logs"
export LOG_MINIO_ACCESS_KEY="minioadmin"
export LOG_MINIO_SECRET_KEY="miniosecret"

# Optional
export VAULT_ROLE_ID="..."
export VAULT_SECRET_ID="..."
export MINIO_ACCESS_KEY="..."        # bundle artifact storage
export MINIO_SECRET_KEY="..."

./dimension-server
```

**In multi-host mode:** The gateway does NOT need Firecracker or KVM. It dispatches VM execution to workers via gRPC.

### Verify

```bash
curl http://localhost:3000/health
# → {"status": "ok"}
```

Check logs for: `"listening on 0.0.0.0:3000"` and successful Postgres connection.

---

## Worker Deployment

### Prerequisites per Worker Node

- Linux with **KVM support** (bare metal or nested virtualization)
- **Firecracker binary** installed at configured path
- **Kernel binary** (vmlinux) at configured path
- Network connectivity to:
  - Gateway HTTP port (for registration): `http://gateway:3000`
  - Clickhouse HTTP port (for inserts): `http://clickhouse:8123`
  - MinIO port (for log uploads): `http://minio:9000`
- If `DIMENSION_ENABLE_NETWORK=true`: root privileges and `ip`/`iptables` commands

### Start

```bash
export DIMENSION_GATEWAY_URL="http://gateway-host:3000"
export DIMENSION_WORKER_GRPC_ADDR="0.0.0.0:50051"
export DIMENSION_WORKER_ADVERTISE_ADDR="worker-1.internal:50051"
export DIMENSION_WORKER_MEMORY_MB=16384
export DIMENSION_WORKER_VCPUS=8
export DIMENSION_KERNEL_PATH="/opt/hyphae/kernel/vmlinux"
export DIMENSION_FIRECRACKER_BIN="/usr/local/bin/firecracker"
export DIMENSION_ENABLE_NETWORK=true

# Observability
export CLICKHOUSE_URL="http://clickhouse-host:8123"
export CLICKHOUSE_DATABASE="dimension"
export LOG_MINIO_ENDPOINT="http://minio-host:9000"
export LOG_MINIO_BUCKET="dimension-logs"
export LOG_MINIO_ACCESS_KEY="minioadmin"
export LOG_MINIO_SECRET_KEY="miniosecret"

./dimension-worker
```

### What the Worker Does on Startup

1. Starts gRPC server on port 50051
2. Connects to Clickhouse and runs `ensure_table()` (creates `invocations` table if missing)
3. Initializes LogUploader (MinIO) if credentials are set
4. Registers with the gateway via HTTP POST to `/internal/workers/register` (retries with exponential backoff)
5. Starts heartbeat re-registration loop

### Verify

- Worker logs: `"gRPC server starting"`, `"successfully registered with gateway"`
- Gateway logs: worker appears in `GET /admin/workers` response
- Check: `curl http://gateway:3000/admin/health -H "Authorization: Bearer $TOKEN"`

### Worker Registration Protocol

Workers register via HTTP POST to `{GATEWAY_URL}/internal/workers/register`:

```json
{
  "worker_id": "uuid-string",
  "grpc_addr": "worker-host:50051",
  "capacity": { "memory_mb": 16384, "vcpus": 8 }
}
```

Gateway health-polls each worker via `Health` gRPC RPC every 15 seconds. **3 consecutive misses → worker removed from registry.**

### Worker Scheduling

The gateway uses a **most-available-first** strategy:
1. Filter out draining workers
2. Filter to workers with sufficient memory and vCPUs for the request
3. Pick the one with the most available memory

---

## CLI Setup

### Install

Copy the `dimension-cli` binary to the developer's PATH:

```bash
cp target/release/dimension-cli /usr/local/bin/dimension
```

### Authenticate

```bash
dimension login --api-key YOUR_API_KEY
# Validates against GET /admin/health on the gateway
# Stores credential in ~/.dimension/credentials
```

Token resolution chain: `--token` flag → `DIMENSION_TOKEN` env var → `~/.dimension/credentials` → error.

`dimension login` and `dimension build` do **not** require authentication (handled before token resolution).

---

## Bundle Workflow

### 1. Build

```bash
# From a Dockerfile (recommended)
dimension build --docker myimage:latest
# Output: rootfs ext4 at /tmp/dimension-build-XXXX/rootfs.ext4
# Output: SHA-256 hash printed

# From a project directory with a Dockerfile
dimension build ./my-agent/

# From a pre-compiled binary
dimension build --binary ./my-binary
```

`--embed-agent` defaults to `true`: the `dimension-agent` sidecar is baked into the rootfs. It delivers the payload to the bundle's stdin over vsock and bridges outbound events back to the worker.

### 2. Push

```bash
dimension push --name my-agent --tag latest --file /path/to/rootfs.ext4
```

What happens:
- CLI uploads ext4 via multipart POST to `/bundles/push`
- Gateway computes SHA-256 hash, deduplicates (identical hash → returns existing bundle_id)
- Stores as `sha256-{hash}.ext4` on disk, registers in Postgres images table
- Enforces version retention: **newest 3 per name+owner**, older versions GC'd (DB record + disk file)
- Eagerly distributes to all registered workers via `PushBundle` gRPC (best-effort — failure logged per worker but doesn't fail the push)

### 3. Run

```bash
# Sync mode — blocks, returns stdout
dimension run --bundle my-agent --mode sync --payload '{"key": "value"}'

# Async mode — returns invocation ID
dimension run --bundle my-agent --mode async --payload '{"key": "value"}'
# Output: invocation_id (bare UUID for piping)

# Persistent mode — VM stays up
dimension run --bundle my-agent --mode persistent --payload @payload.json
# Output: invocation_id

# Read payload from file (@ prefix, like curl)
dimension run --bundle my-agent --mode sync --payload @path/to/payload.json
```

### 4. Check Logs

```bash
dimension logs get <invocation_id>
# Output: metadata header (mode, status, duration, bundle) + log content
```

### 5. Stop (Persistent Mode)

```bash
dimension stop <invocation_id>
```

### 6. Rollback

```bash
dimension rollback my-agent
# Swaps active version to the second-newest
# Redistributes the rollback version to all workers
```

---

## Telegram Bridge Deployment

The Telegram bridge is a Node.js/TypeScript service that connects Telegram Bot API to Dimension. It receives Telegram webhook updates, dispatches async invocations to Dimension via POST /run, and delivers agent responses back to Telegram chats. It handles session continuity (one session per chat) and message buffering (queues messages while an invocation is in-flight).

### Architecture

```
Telegram ──webhook──► Cloudflare Tunnel ──► telegram-bridge (:3000)
                                                │
                                          POST /run (async)
                                                │
                                                ▼
                                       Dimension Gateway ──► Worker ──► VM
                                                                         │
                                                                    POST /agent/callback
                                                                         │
                                                                         ▼
                                                              telegram-bridge
                                                                    │
                                                              sendMessage
                                                                    │
                                                                    ▼
                                                                Telegram
```

### Bridge Host Requirements

- **Node.js 22+**
- **npm** (bundled with Node.js)
- Network access to:
  - Dimension gateway HTTP port (default 3000) — for POST /run
  - Internet — for Telegram Bot API calls (api.telegram.org)
- Inbound access from Cloudflare tunnel (or direct internet) on bridge port (default 3000)

Build and run:

```bash
cd telegram-bridge
npm ci
npm run build
node dist/index.js   # or wrap in a systemd unit / process manager
```

### Environment Variables

Provide these via the environment (e.g. a `.env` file sourced by your process manager, or systemd `EnvironmentFile`).

#### Required

| Env Var | Description |
|---|---|
| `TELEGRAM_BOT_TOKEN` | Bot token from BotFather (format: `123456:ABC-DEF...`) |
| `DIMENSION_API_URL` | Dimension gateway URL (e.g. `http://gateway:3000`) |
| `DIMENSION_API_KEY` | Dimension API bearer token |
| `DIMENSION_BUNDLE_ID` | Bundle name to invoke for Telegram messages |

#### Optional

| Env Var | Default | Description |
|---|---|---|
| `BRIDGE_PORT` | `3000` | HTTP listen port |
| `MINIO_ENDPOINT` | — | S3-compatible endpoint (passed through to agent VM) |
| `MINIO_BUCKET` | — | MinIO bucket name |
| `MINIO_ACCESS_KEY` | — | MinIO access key |
| `MINIO_SECRET_KEY` | — | MinIO secret key |

### Cloudflare Tunnel Configuration

The bridge must be reachable from the internet so Telegram can deliver webhook updates. Use a Cloudflare tunnel to expose the bridge without opening inbound ports.

1. **Install cloudflared** on the bridge host:
   ```bash
   curl -L https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 \
     -o /usr/local/bin/cloudflared
   chmod +x /usr/local/bin/cloudflared
   ```

2. **Authenticate and create tunnel:**
   ```bash
   cloudflared tunnel login
   cloudflared tunnel create telegram-bridge
   ```

3. **Configure the tunnel** (`~/.cloudflared/config.yml`):
   ```yaml
   tunnel: <TUNNEL_ID>
   credentials-file: /root/.cloudflared/<TUNNEL_ID>.json

   ingress:
     - hostname: bridge.yourdomain.com
       service: http://localhost:3000
     - service: http_status:404
   ```

4. **Add DNS record:**
   ```bash
   cloudflared tunnel route dns telegram-bridge bridge.yourdomain.com
   ```

5. **Run the tunnel** (or install as a systemd service):
   ```bash
   cloudflared tunnel run telegram-bridge

   # Or install as systemd service:
   cloudflared service install
   ```

6. **Verify** the tunnel is reachable:
   ```bash
   curl https://bridge.yourdomain.com/health
   # → {"status":"ok","uptime":...}
   ```

### Telegram Bot Webhook Registration

After the bridge is deployed and the Cloudflare tunnel is active, register the webhook URL with Telegram using the Bot API `setWebhook` method:

```bash
# Replace YOUR_BOT_TOKEN and TUNNEL_URL with actual values
curl -s "https://api.telegram.org/botYOUR_BOT_TOKEN/setWebhook" \
  -d "url=https://bridge.yourdomain.com/telegram/webhook"
# → {"ok":true,"result":true,"description":"Webhook was set"}
```

**Verify the webhook is registered:**

```bash
curl -s "https://api.telegram.org/botYOUR_BOT_TOKEN/getWebhookInfo" | jq .
# Check: url matches your tunnel URL, pending_update_count is 0, no errors
```

**Remove the webhook** (if needed):

```bash
curl -s "https://api.telegram.org/botYOUR_BOT_TOKEN/setWebhook" -d "url="
```

### Bridge Verification Checklist

After deployment, run the automated verification script:

```bash
./scripts/verify-e2e-bridge.sh https://bridge.yourdomain.com
```

The script checks:

- [ ] `GET /health` returns 200 with `{"status":"ok"}`
- [ ] `POST /telegram/webhook` accepts a TelegramUpdate and returns `{"ok":true}`
- [ ] `POST /agent/callback` accepts an AgentCallbackEvent and returns `{"ok":true}`

Then complete the manual verification:

- [ ] Cloudflare tunnel is active and routes to bridge port 3000
- [ ] Telegram webhook registered via `setWebhook` API
- [ ] `getWebhookInfo` shows correct URL and no errors
- [ ] Send a message to the bot in Telegram → bridge logs show `telegram webhook received`
- [ ] Dimension gateway receives POST /run → worker launches VM
- [ ] Agent callback arrives at bridge → bot replies in Telegram
- [ ] Send a second message → agent response reflects session history
- [ ] Send two rapid messages → second is buffered until first invocation completes

### Service Management

```bash
# Status
sudo systemctl status telegram-bridge

# Restart
sudo systemctl restart telegram-bridge

# Logs (follow)
sudo journalctl -u telegram-bridge -f

# Check bridge health
curl http://localhost:3000/health
```

---

## Verification Checklist

After deploying, verify the following:

### Gateway

- [ ] `curl http://gateway:3000/health` returns 200
- [ ] Postgres connection established (check logs for migration output)
- [ ] First-run admin API key captured from stderr
- [ ] Workers appear in `/admin/workers` response (if multi-host)

### Worker

- [ ] Worker logs show `"successfully registered with gateway"`
- [ ] Gateway logs show `"worker health ok"` during health polls
- [ ] KVM available: `ls /dev/kvm` exists
- [ ] Firecracker binary accessible: `firecracker --version`
- [ ] Kernel binary exists at `DIMENSION_KERNEL_PATH`

### End-to-End

```bash
# 1. Login
dimension login --api-key $API_KEY

# 2. Build a test bundle (needs Docker)
mkdir /tmp/test-agent && cd /tmp/test-agent
cat > Dockerfile <<'EOF'
FROM alpine:3.19
RUN apk add --no-cache jq
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
EOF
cat > entrypoint.sh <<'EOF'
#!/bin/sh
# Read payload from stdin (delivered via MMDS)
PAYLOAD=$(cat)
echo "Received: $PAYLOAD"
EOF
dimension build --docker test-agent:latest

# 3. Push
dimension push --name test-agent --tag latest --file /path/to/rootfs.ext4

# 4. Run (sync mode)
dimension run --bundle test-agent --mode sync --payload '{"hello": "world"}'
# Expected output: "Received: {"hello": "world"}"

# 5. Run (async mode)
INVOCATION_ID=$(dimension run --bundle test-agent --mode async --payload '{"hello": "world"}')

# 6. Check logs (wait a few seconds for completion)
dimension logs get $INVOCATION_ID
```

---

## API Reference

### Public Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | None | Health check |

### Protected Endpoints (Bearer Token)

| Method | Path | Description |
|---|---|---|
| `POST` | `/run` | Run a bundle invocation (sync/async/persistent) |
| `GET` | `/runs/{id}/events` | SSE stream of run events (`state` + `bundle`; replay via `Last-Event-ID`) |
| `POST` | `/run/{id}/stop` | Stop a running invocation |
| `GET` | `/invocations/{id}` | Get invocation metadata (+ logs with `?include_logs=true`) |
| `POST` | `/bundles/push` | Upload a content-hashed ext4 rootfs |
| `POST` | `/bundles/upload` | Upload a Docker image tar (legacy, triggers server-side conversion) |
| `POST` | `/bundles/{name}/rollback` | Rollback bundle to previous version |
| `GET` | `/bundles` | List bundles |
| `GET` | `/bundles/{id}` | Get bundle details |

### Admin Endpoints (`/admin/*`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/admin/health` | Admin health check (used by `dimension login` validation) |
| `GET` | `/admin/workers` | List registered workers |
| `POST` | `/admin/workers/{id}/drain` | Drain a worker |
| `DELETE` | `/admin/workers/{id}` | Remove a worker |
| `GET` | `/admin/bundles` | List all bundles |
| `DELETE` | `/admin/bundles/{id}` | Delete a bundle |
| `POST` | `/admin/bundles/{id}/rebuild` | Rebuild a bundle |
| `POST` | `/admin/users` | Create a user |
| `GET` | `/admin/users` | List users |
| `DELETE` | `/admin/users/{id}` | Delete a user |
| `POST` | `/admin/users/{id}/keys` | Create API key |
| `GET` | `/admin/users/{id}/keys` | List API keys |
| `POST` | `/admin/users/{id}/keys/{kid}/revoke` | Revoke API key |

### Internal Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/internal/workers/register` | Worker self-registration |

### POST /run Request Body

```json
{
  "bundle_id": "my-agent",
  "mode": "sync",
  "payload": { "any": "json" },
  "user": "user-uuid"
}
```

**mode values:** `sync`, `async`, `persistent`

### POST /run Response

- **sync**: `200` with stdout as response body
- **async/persistent**: `202` with `{"invocation_id": "uuid"}`

### GET /invocations/{id} Response

```json
{
  "invocation_id": "uuid",
  "user_id": "uuid",
  "bundle_id": "my-agent",
  "worker_id": "uuid",
  "mode": "async",
  "status": "completed",
  "exit_code": 0,
  "duration_ms": 1234,
  "log_url": "invocations/uuid/output.log",
  "created_at": 1712100000000,
  "completed_at": 1712100001234,
  "logs": "stdout content here..."
}
```

The `logs` field is only present when `?include_logs=true` is set.

---

## Observability

### Clickhouse Invocations

Query invocation history directly:

```sql
SELECT invocation_id, bundle_id, mode, status, exit_code, duration_ms
FROM invocations
ORDER BY created_at DESC
LIMIT 20;
```

### MinIO Logs

Logs are stored at: `dimension-logs/invocations/{invocation_id}/output.log`

Browse via MinIO Console at `:9001` or mc CLI:

```bash
mc cat local/dimension-logs/invocations/{id}/output.log
```

### Structured Logging

All services use `tracing` with structured fields. Set `RUST_LOG=info` (default) or `RUST_LOG=debug` for verbose output.

Key log patterns to monitor:
- **Gateway**: `"POST /run"` with invocation_id, bundle_id, mode
- **Worker**: `"RunInvocation"` lifecycle phases (launch, vsock-connect, stdout-read, process-exit, cleanup)
- **Worker**: `"CH insert"` and `"MinIO upload"` for observability pipeline health
- **Worker**: `"PushBundle"` for bundle distribution events

### Fire-and-Forget Observability

Both Clickhouse inserts and MinIO uploads use **fire-and-forget** semantics (`tokio::spawn`). A Clickhouse or MinIO outage will **never block or fail an invocation**. Failures are logged with invocation_id context for manual recovery.

---

## Troubleshooting

### Worker Not Registering

- Check `DIMENSION_GATEWAY_URL` points to the correct gateway address
- Check network connectivity: `curl $DIMENSION_GATEWAY_URL/health`
- Check gateway logs for registration attempts
- Worker retries with exponential backoff (1s → 30s max) — give it time

### Invocation Fails with "no available workers"

- Check `curl http://gateway:3000/admin/workers` — are workers listed?
- Check worker has sufficient memory/vCPUs for the request
- Check worker is not draining

### Sync Mode Returns Empty

- Ensure the bundle's entrypoint writes to stdout (the request payload arrives on stdin)
- Ensure the bundle was built with `--embed-agent` (the default) — without `dimension-agent` in the rootfs there is no stdin/stdout bridge
- Check worker logs for vsock connection errors

### Logs Missing / `dimension logs` Returns No Content

- Check worker has `CLICKHOUSE_URL` and `LOG_MINIO_*` configured
- Check Clickhouse is accessible from the worker
- Check MinIO bucket `dimension-logs` exists
- Check gateway has `CLICKHOUSE_URL` and `LOG_MINIO_*` configured (for retrieval)
- Logs are only uploaded after invocation completes — in-progress invocations have no logs yet

### Bundle Push Succeeds but Worker Doesn't Have It

- Bundle distribution is **best-effort** — check worker logs for `PushBundle` errors
- Re-push the bundle or push a new version (triggers redistribution)
- Verify worker gRPC port 50051 is reachable from the gateway

### Payload Not Reaching Guest

- The payload is delivered over vsock by `dimension-agent` — verify the bundle was built with `--embed-agent` (the default)
- Check worker logs for vsock UDS errors (`v.sock` in the runtime dir)
- Check hyphae-init logs inside the VM (if accessible via serial console)

### Clickhouse Table Not Created

- Worker runs `ensure_table()` on startup — check logs for CH connection errors
- Verify `CLICKHOUSE_URL` is correct and Clickhouse is running
- Manually create: connect to CH at port 8123 and run the CREATE TABLE statement from the [Database section](#clickhouse-table)

---

## What Changed in M003

> **Historical record.** This section describes the M003 refactor as it
> shipped. Several removals were since reversed by the outbound message
> center: `dimension-agent`, `dimension-protocol`, Pulsar, SSE streaming, and
> the agent event socket are all back (payload over vsock, events via
> UDS → vsock → Pulsar/Clickhouse → `GET /runs/{id}/events`). The
> telegram-bridge returned as a TypeScript service. Webhook dispatch and
> `POST /messages` remain removed for good — SSE is the only response channel.

### Removed

| Component | What It Did |
|---|---|
| `dimension-agent` (crate) | In-VM vsock agent for protobuf IPC |
| `telegram-bridge` (crate) | Telegram Bot API bridge |
| `dimension-protocol` (crate) | Protobuf definitions for vsock IPC |
| `dispatch.rs` | Gateway-side webhook dispatch |
| Session/message handlers | POST /messages, session management |
| TypeScript SDK IPC transport | Agent ↔ platform communication via Unix socket |
| SSE streaming | Server-sent events for response streaming |
| Task orchestration | Goal-driven task scheduler |
| Pulsar integration | Event-driven agent invocation |
| A2A protocol endpoints | Inter-agent JSON-RPC communication |

### Added

| Component | What It Does |
|---|---|
| `POST /run` | Single endpoint for sync/async/persistent invocations |
| `POST /run/{id}/stop` | Stop persistent/async invocations |
| `GET /invocations/{id}` | Invocation metadata + log retrieval |
| `POST /bundles/push` | Content-hashed ext4 upload with dedup and version retention |
| `POST /bundles/{name}/rollback` | Version swap + redistribution |
| MMDS payload delivery | JSON via Firecracker metadata service (replaces vsock+protobuf) |
| Vsock stdout return | Raw stdout bytes for sync mode (no agent, no protobuf) |
| `RunInvocation` gRPC RPC | Worker-side invocation execution |
| `StopInvocation` gRPC RPC | Worker-side invocation termination |
| `PushBundle` gRPC RPC | Worker-side bundle receipt |
| Clickhouse integration | Invocation metadata tracking |
| MinIO log integration | Stdout/stderr capture and upload |
| `dimension login` | CLI credential storage |
| `dimension build` | Local rootfs build from Dockerfile |
| `dimension push` | Bundle upload with progress bar |
| `dimension run` | Invocation dispatch |
| `dimension stop` | Persistent VM termination |
| `dimension logs get` | Log retrieval |
| `dimension rollback` | Version rollback |

### Config Changes

**New env vars to add:**

| Env Var | Where | Why |
|---|---|---|
| `CLICKHOUSE_URL` | Gateway + Worker | Invocation tracking |
| `CLICKHOUSE_DATABASE` | Gateway + Worker | Invocation tracking |
| `LOG_MINIO_ENDPOINT` | Gateway + Worker | Log storage |
| `LOG_MINIO_BUCKET` | Gateway + Worker | Log storage |
| `LOG_MINIO_ACCESS_KEY` | Gateway + Worker | Log storage auth |
| `LOG_MINIO_SECRET_KEY` | Gateway + Worker | Log storage auth |

**Env vars no longer used:**

| Env Var | Why |
|---|---|
| `DIMENSION_MAX_CONCURRENT_TASKS_PER_USER` | Task system removed |
| `DIMENSION_HEARTBEAT_INTERVAL` | SSE heartbeat removed |

**Re-added since M003** (outbound message center): `PULSAR_URL` and
`PULSAR_TOPIC` on both gateway and worker for live run-event streaming.

### Infrastructure Changes

- **Add:** Clickhouse container (port 8123) — new dependency
- **Add:** MinIO bucket `dimension-logs` — new bucket for invocation logs (separate from bundle artifact bucket `dimension`)
- **Keep:** PostgreSQL — still required for user/session/bundle metadata
- **Keep:** Garage/MinIO bundle storage — still used for bundle artifacts
- **Keep:** Vault — still used for per-VM secret management
- **Remove:** Pulsar container — no longer needed

### Migration Path

1. Add Clickhouse container to Docker Compose
2. Create `dimension-logs` MinIO bucket
3. Add `CLICKHOUSE_*` and `LOG_MINIO_*` env vars to gateway and worker env files
4. Remove `DIMENSION_PULSAR_URL` from gateway env file
5. Build updated binaries (`cargo build --release`)
6. Deploy updated gateway binary — Postgres migrations run automatically
7. Deploy updated worker binary
8. Verify: `dimension run --bundle test --mode sync --payload '{}'` works end-to-end
