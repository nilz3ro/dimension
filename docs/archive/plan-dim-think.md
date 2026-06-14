# Dim Think — Architecture Plan

## Vision

Dimension becomes a **dumb compute launcher**. It takes a user, a bundle ID, and a JSON payload, boots a microVM, delivers the payload, and gets out of the way. Agents and serverless functions manage their own state and communicate results back to their bridges and applications via outbound HTTP. Dimension is not in the data path for responses.

This is conceptually similar to AWS Lambda — Dimension is a serverless compute platform optimized for autonomous agents running in isolated microVMs.

---

## Core Concepts

### Bundles

A bundle is a rootfs image built from a Docker container. It contains everything an agent or serverless function needs to run. Bundles are built locally via the Dimension CLI, uploaded to Dimension, and distributed to workers.

**Versioning:** Latest-wins. When a new version is pushed, workers evict the old version and serve the new one. The last N versions are retained for rollback via `dimension rollback <bundle_name>`. Bundles are content-hashed for deduplication and reproducibility.

### Bridges

Bridges are the connectors between where the work originates (chat apps, web apps, APIs, other systems) and Dimension. Bridges are mostly stateless. They call Dimension's gateway to trigger bundle invocations and receive results asynchronously — the agent code in the bundle POSTs results back to the bridge or any other endpoint via outbound HTTP.

Bridge and agent authors define their own communication protocol. Dimension does not impose a structure on the payload or on how results are delivered back. This is an agreement between your bridge application and your agents.

### User Tiers

- **Chat users:** End users interacting through bridges. No access to Dimension infrastructure.
- **API key users (developers):** Build agents and bridges. Access to the CLI, bundle management, invocation logs, and all developer-facing endpoints.

---

## API Surface

### Run an Invocation

```
POST /run
{
  "user": "user_id",
  "bundle_id": "bundle_name",
  "payload": {
    "session_id": "abc123",
    "other_thing": "value"
  }
}

Response (202):
{
  "invocation_id": "inv_abc123"
}
```

Dimension picks a worker, delivers the JSON payload to the guest VM via vsock (through the hyphae init binary), and returns immediately with an invocation ID. The payload is opaque to Dimension — it passes it through unchanged.

The agent code inside the VM can do whatever it wants: call back to the bridge, hit external APIs, download session state from an object store, run an AI framework. Dimension doesn't care.

### Query Invocation Logs (Phase 3)

```
GET /invocations/{invocation_id}/logs
```

Returns stdout/stderr and metadata (exit code, duration, timestamps) for a given invocation.

---

## Request Flow

```
Bridge/App
  → POST /run {user, bundle_id, payload}
    → Gateway (202, returns invocation_id)
      → Worker boots/resumes VM
        → Payload delivered via vsock to hyphae init
          → Agent code runs
            → Agent reaches out to bridge/services via outbound HTTP
```

Dimension is a hub-and-spoke model. Many bridges and applications call bundles through the gateway. The gateway dispatches to workers. Agents communicate results outward on their own.

---

## Networking & Security

- Guest VMs have internet access **only** through the TAP bridge. No LAN access.
- If a bundle VM needs to access something on the internal network, that service must be placed behind a **Cloudflare Tunnel**. The agent accesses it over the public internet through the tunnel. This eliminates any LAN attack surface from guest VMs.
- Future hosted services (object storage, vLLM inference) will also sit behind Cloudflare Tunnels.

---

## What Dimension Does

- Accepts run requests, dispatches to workers, delivers payloads via vsock
- Returns an invocation ID
- Manages bundle builds, uploads, versioning, and distribution to workers
- Captures invocation logs (stdout/stderr/exit code) for developer debugging
- Manages users and API keys

## What Dimension Does NOT Do

- Session management — agents manage their own state
- Message or tool call tracking — that's the agent's concern
- Response routing — agents call back to bridges directly
- Impose opinions on agent ↔ bridge communication protocols

This means agents can use any AI framework (LangChain, AutoGen, custom code, etc.) without needing a Dimension SDK. The only interface is: receive JSON via vsock, do your thing, call out over HTTP.

---

## Data Layer

### Postgres (Relational)

Used for entities with relationships and transactional requirements:

- **Users** — id, tier (chat/developer), created_at
- **API keys** — id, user_id, key_hash, scopes, created_at
- **Bundles** — id, user_id, name, current_version_hash
- **Bundle versions** — id, bundle_id, version_num, content_hash, size, created_at
- **Invocations** — id, user_id, bundle_id, worker_id, status, created_at (minimal tracking record)

### Clickhouse (Append-Only, Phase 3)

Used for high-volume log and event data:

- **Invocation logs** — invocation_id, timestamp, stream (stdout/stderr), line
- **Metrics and analytics** — invocation counts, durations, error rates

---

## CLI

### `dimension build`

Builds a bundle from a Dockerfile or directory. Uses Docker to build the image, extracts the rootfs, and prepares it for upload. This is the only way to create bundles.

### `dimension push <bundle_name>`

Uploads the built bundle to Dimension. The bundle is content-hashed and synced to workers. Workers evict the previous version so there are no stale cache issues. The last N versions are retained.

### `dimension rollback <bundle_name>`

Rolls back to the previous bundle version.

### `dimension logs <invocation_id>` (Phase 3)

Tails invocation logs from Clickhouse.

---

## Build Phases

### Phase 1: Bundle CLI + Run Endpoint

1. `dimension build` — Dockerfile → Docker image → rootfs extraction → bundle artifact
2. `dimension push` — upload bundle to Dimension, distribute to workers, evict old versions
3. `POST /run` endpoint — gateway accepts request, dispatches to worker, delivers payload via vsock, returns invocation_id (202)
4. Worker bundle cache invalidation — new pushes replace the active version on all workers

### Phase 2: Auth + Users

1. User registration and management
2. API key generation and authentication
3. Scope enforcement (developer vs chat user capabilities)
4. Auth on all gateway and CLI endpoints

### Phase 3: Observability

1. Clickhouse deployment and schema
2. Log capture pipeline — worker collects stdout/stderr, ships to Clickhouse
3. `GET /invocations/{id}/logs` endpoint
4. `dimension logs <invocation_id>` CLI command

### Future Considerations

- **Synchronous response path:** For simple compute functions that want request/response semantics, allow the VM to write a response back through vsock that the gateway returns on the original HTTP call. Not needed yet — all current use cases are async.
- **Hosted object storage:** For agent state/session persistence, behind CF tunnels.
- **Hosted vLLM endpoint:** Shared inference for agents, behind CF tunnels.
- **Payload size limits:** JSON payloads will need a size cap. For large binary data (images, audio), the pattern is to upload to object storage and pass the URL in the payload.
- **Log retention policies:** Per-tier retention windows in Clickhouse.
- **Egress controls:** Rate limiting or allowlisting outbound traffic from guest VMs.
