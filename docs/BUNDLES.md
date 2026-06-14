# Building and Deploying Hyphae Bundles

This guide covers building a Docker-based agent, configuring it with `dimension.toml`, and uploading it to a Dimension gateway.

---

## Table of Contents

1. [What's a Bundle?](#whats-a-bundle)
2. [Quick Start](#quick-start)
3. [Project Structure](#project-structure)
4. [The dimension.toml Manifest](#the-dimensiontoml-manifest)
5. [Writing the Dockerfile](#writing-the-dockerfile)
6. [The Entrypoint Script](#the-entrypoint-script)
7. [Building the Docker Image](#building-the-docker-image)
8. [Uploading to Dimension](#uploading-to-dimension)
9. [Sending Messages to Your Bundle](#sending-messages-to-your-bundle)
10. [Using the Dimension SDK (Optional)](#using-the-dimension-sdk-optional)
11. [Advanced: A2A Agent Discovery](#advanced-a2a-agent-discovery)
12. [Advanced: Persistent Volumes](#advanced-persistent-volumes)
13. [Troubleshooting](#troubleshooting)
14. [Reference: Full dimension.toml Schema](#reference-full-dimensiontoml-schema)
15. [Reference: API Endpoints](#reference-api-endpoints)

---

## What's a Bundle?

A bundle is a Docker image that runs inside a Firecracker microVM. When someone dispatches a run against your bundle via the Dimension API, the gateway:

1. Boots a fresh microVM from your bundle's rootfs image
2. Connects to it over vsock (a VM socket — not a network socket)
3. Sends the request payload as JSON on your agent's stdin
4. Streams everything your agent emits (`dimension.send(...)` events, stdout, stderr) out as run events
5. Tears down the VM

Every run gets its own isolated VM. Your code can't interfere with other bundles or other runs.

```
Client → POST /run → Gateway → Firecracker VM (your code)
Client ← SSE GET /runs/{id}/events ← Gateway ← run events ← your code
```

---

## Quick Start

If you want to get something running fast, here's the minimum:

```
my-agent/
├── agent.js          # Your code (reads stdin, writes to stdout)
├── Dockerfile        # Standard Docker build
├── dimension.toml    # Bundle manifest
└── entrypoint.sh     # VM boot script
```

**agent.js** — a minimal agent:
```js
const input = await new Promise((resolve) => {
  const chunks = [];
  process.stdin.on("data", (chunk) => chunks.push(chunk));
  process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString()));
});

const request = JSON.parse(input);
const userText = request.content
  .filter(b => b.type === "text")
  .map(b => b.text)
  .join("\n");

// Your logic here
const response = `You said: ${userText}`;

// Write response to stdout — the gateway captures this
process.stdout.write(response);
```

**dimension.toml**:
```toml
[resources]
memory_mb = 256
vcpus = 1
timeout_secs = 30
```

**entrypoint.sh**:
```sh
#!/bin/sh
if [ -f /etc/resolv.conf.vm ]; then
    cp /etc/resolv.conf.vm /etc/resolv.conf 2>/dev/null || true
fi
if [ -f /etc/hyphae/env ]; then
    set -a
    . /etc/hyphae/env
    set +a
fi
exec node /app/agent.js
```

**Dockerfile**:
```dockerfile
FROM node:22-slim

RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

# DNS config for inside the VM
RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

WORKDIR /app
COPY agent.js .
COPY entrypoint.sh .
COPY dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x entrypoint.sh

ENTRYPOINT ["/app/entrypoint.sh"]
```

Build, save, upload:
```sh
# Build the Docker image
docker build -t my-agent .

# Export as a tar file
docker save my-agent -o my-agent.tar

# Upload to Dimension
curl -X POST "http://<gateway>:3000/bundles/upload?name=my-agent&tag=latest" \
  -H "Authorization: Bearer $DIMENSION_TOKEN" \
  -F "image=@my-agent.tar"
```

---

## Project Structure

A bundle project has four essential files:

| File | Required | Purpose |
|------|----------|---------|
| `Dockerfile` | Yes | Builds the Docker image |
| `dimension.toml` | Recommended | Declares resources, capabilities, and metadata |
| `entrypoint.sh` | Yes | Boot script that runs as PID 1 inside the VM |
| Your code | Yes | The actual agent logic |

The `dimension.toml` **must be copied to `/etc/hyphae/dimension.toml`** inside the Docker image. The gateway reads it from the extracted image layers during the upload process.

---

## The dimension.toml Manifest

The manifest declares what your bundle needs. All sections are optional — missing sections use safe defaults.

### Minimal example

```toml
[resources]
memory_mb = 512
vcpus = 1
timeout_secs = 120
```

### Full example

```toml
[resources]
memory_mb = 2048       # VM memory in MB (default: 256)
vcpus = 2              # Number of virtual CPUs (default: 2)
timeout_secs = 300     # Max execution time before VM is killed (server default if omitted)

[capabilities]
storage = true         # Access to persistent object storage (default: false)
agent_calls = true     # Ability to call other agents via A2A (default: false)
secrets = false        # Access to the secrets service (default: false)
tokenize = false       # Access to tokenize/detokenize endpoints (default: false)

[env]
VLLM_BASE_URL = "http://100.104.71.102:8000"
VLLM_MODEL = "openai/gpt-oss-20b"

[secrets]
names = ["OPENAI_API_KEY", "GITHUB_TOKEN"]

[volumes]
size = "10GB"          # Persistent volume mounted at /workspace

[a2a]
name = "my-agent"
description = "Does useful things with text"
skills = ["text-processing", "summarization"]
```

### Section reference

#### `[resources]`

Controls VM sizing. Larger values = more cost, faster execution.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `memory_mb` | integer | 256 | VM memory in megabytes |
| `vcpus` | integer | 2 | Virtual CPU count |
| `timeout_secs` | integer | server default | Max seconds before the VM is killed |

#### `[capabilities]`

Default-deny capability gates. Each must be explicitly set to `true`.

| Field | Default | What it enables |
|-------|---------|-----------------|
| `storage` | `false` | Read/write to persistent object storage via the SDK |
| `agent_calls` | `false` | Call other Dimension agents from inside your agent |
| `secrets` | `false` | Read secrets stored in the platform secrets service |
| `tokenize` | `false` | Access to the tokenization/detokenization service |

#### `[env]`

Flat key-value pairs injected as environment variables at VM boot. These are applied **after** Docker `ENV` directives, so they override anything baked into the image.

```toml
[env]
MY_VAR = "my_value"
API_BASE = "https://api.example.com"
```

> **Security note:** Don't put secrets in `[env]` — they're stored in plain text in the registry. Use `[secrets]` instead.

#### `[secrets]`

Declares which secrets the bundle requires. The platform checks availability at launch and injects them as environment variables. Values are never stored in the manifest.

```toml
[secrets]
names = ["OPENAI_API_KEY", "DATABASE_URL"]
```

#### `[volumes]`

Declares a persistent ext4 volume mounted at `/workspace`. Data persists across sessions.

```toml
[volumes]
size = "10GB"
```

| Field | Description |
|-------|-------------|
| `size` | Human-readable size string (e.g. `"512MB"`, `"10GB"`) |
| `shared_mount` | Name of a user-owned named volume to mount instead of a session-scoped one |

#### `[a2a]`

Agent-to-Agent metadata. Makes your bundle discoverable by other agents and external callers.

```toml
[a2a]
name = "my-agent"
description = "Translates text between languages"
skills = ["translation", "language-detection"]
```

When `[a2a]` is present, the gateway generates an agent card at `GET /agents/{name}/` following the A2A protocol.

---

## Writing the Dockerfile

The Dockerfile is a standard Docker build. A few things are specific to the Hyphae environment:

### Required patterns

1. **DNS configuration** — the VM has no DHCP. Bake a resolv.conf:
   ```dockerfile
   RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm
   ```

2. **Manifest placement** — copy `dimension.toml` to where the gateway expects it:
   ```dockerfile
   COPY dimension.toml /etc/hyphae/dimension.toml
   ```

3. **Entrypoint script** — must be executable:
   ```dockerfile
   COPY entrypoint.sh /app/entrypoint.sh
   RUN chmod +x /app/entrypoint.sh
   ENTRYPOINT ["/app/entrypoint.sh"]
   ```

### Example: Node.js agent

```dockerfile
FROM node:22-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci

COPY src/ src/
RUN npm run build

COPY entrypoint.sh /app/entrypoint.sh
COPY dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

ENTRYPOINT ["/app/entrypoint.sh"]
```

### Example: Python agent

```dockerfile
FROM python:3.12-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY agent.py .
COPY entrypoint.sh /app/entrypoint.sh
COPY dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

ENTRYPOINT ["/app/entrypoint.sh"]
```

### Image size considerations

The Docker image is converted to an ext4 rootfs. Larger images = longer boot times. Tips:

- Use slim/alpine base images
- Multi-stage builds to exclude build tools from the final image
- `--no-install-recommends` on apt
- `rm -rf /var/lib/apt/lists/*` after apt
- Don't include test files, docs, or development dependencies

---

## The Entrypoint Script

The entrypoint script is the first thing that runs when the VM boots. It handles environment setup and then `exec`s your agent.

### Standard template

```sh
#!/bin/sh

# 1. Apply DNS config (VM has no DHCP)
if [ -f /etc/resolv.conf.vm ]; then
    cp /etc/resolv.conf.vm /etc/resolv.conf 2>/dev/null || true
fi

# 2. Load runtime environment variables from the platform
#    (secrets, env vars from dimension.toml, etc.)
if [ -f /etc/hyphae/env ]; then
    set -a
    . /etc/hyphae/env
    set +a
fi

# 3. Run your agent (use exec to replace the shell process)
exec node /app/dist/agent.js
```

The three steps matter:

1. **DNS** — without this, network calls fail with name resolution errors
2. **Environment** — the platform writes runtime secrets and env vars to `/etc/hyphae/env`. If you skip this, your agent won't see secrets declared in `[secrets]` or env vars from `[env]`
3. **exec** — replaces the shell with your process so signals (SIGTERM on timeout) go directly to your agent

---

## Building the Docker Image

```sh
# Standard Docker build
docker build -t my-agent:latest .

# Export as a tar file (this is what the gateway accepts)
docker save my-agent:latest -o my-agent.tar
```

The gateway accepts the **`docker save` format** — a tar containing the image layers and metadata. Not `docker export` (that's a running container's filesystem).

---

## Uploading to Dimension

### Prerequisites

You need:
- A running Dimension gateway (`http://<gateway>:3000`)
- An API key (either the admin `DIMENSION_TOKEN` or a user API key)

### Upload the image

```sh
curl -X POST "http://<gateway>:3000/bundles/upload?name=my-agent&tag=latest" \
  -H "Authorization: Bearer $DIMENSION_TOKEN" \
  -F "image=@my-agent.tar"
```

**Query parameters:**

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `name` | No | Derived from image name | Bundle name in the registry |
| `tag` | No | `latest` | Version tag |
| `platform` | No | `false` | If `true`, creates a shared platform bundle (admin only) |

**Response (202 Accepted):**

```json
{
  "job_id": "a1b2c3d4-...",
  "status": "queued",
  "poll_url": "/bundles/jobs/a1b2c3d4-..."
}
```

The upload is **asynchronous**. The gateway accepts the tar, then converts it to a Firecracker rootfs image in the background.

### Poll for completion

```sh
curl "http://<gateway>:3000/bundles/jobs/<job_id>" \
  -H "Authorization: Bearer $DIMENSION_TOKEN"
```

**Stages:**

| Stage | Description |
|-------|-------------|
| `queued` | Job accepted, waiting to start |
| `extracting` | Validating and extracting the tar archive |
| `converting` | Building the ext4 rootfs from Docker layers |
| `complete` | Bundle registered and ready to use |
| `failed` | Something went wrong (check `error` field) |

**Complete response:**

```json
{
  "job_id": "a1b2c3d4-...",
  "status": "complete",
  "bundle_id": 42
}
```

### List your bundles

```sh
curl "http://<gateway>:3000/bundles" \
  -H "Authorization: Bearer $DIMENSION_TOKEN"
```

Returns your bundles plus any platform bundles (shared):

```json
{
  "bundles": [
    {
      "id": 42,
      "name": "my-agent",
      "tag": "latest",
      "content_hash": "abc123...",
      "owner_id": "your-user-uuid",
      "size_bytes": 104857600,
      "created_at": 1711929600,
      "default_vcpus": 1,
      "default_memory_mib": 256
    }
  ],
  "next_cursor": null
}
```

### Size limits

- **Upload:** 2 GiB max tar file
- **Extraction:** 8 GiB total extracted, 4 GiB per entry
- **Security:** symlinks, hardlinks, path traversal, and absolute paths are rejected

---

## Running Your Bundle

Once pushed, dispatch runs using the bundle name. `mode: "sync"` blocks and returns the agent's stdout; `mode: "async"` returns immediately with an event stream URL.

```sh
curl -X POST "http://<gateway>:3000/run" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $DIMENSION_TOKEN" \
  -d '{
    "bundle_id": "my-agent",
    "mode": "async",
    "payload": {
      "role": "user",
      "content": [{"type": "text", "text": "Hello, agent!"}],
      "session_id": "any-uuid-you-manage",
      "history": []
    }
  }'
```

**Response (202 Accepted):**

```json
{
  "invocation_id": "uuid-of-run",
  "status": "pending",
  "events_url": "/runs/<invocation_id>/events"
}
```

### Consuming results over SSE

Subscribe to the `events_url` to receive everything the run produces:

```sh
curl -N "http://<gateway>:3000/runs/$RUN_ID/events" \
  -H "Authorization: Bearer $DIMENSION_TOKEN" \
  -H "Accept: text/event-stream"
```

Two event kinds arrive:

- **`state`** — lifecycle phases emitted by the worker (`started`, `completed`, `process_exited`, …). Use the terminal phase to know the run is done.
- **`bundle`** — whatever your agent sent via `dimension.send(...)`. With `dimension.sendMessage(text)` the body looks like:

```json
{
  "session_id": "...",
  "event_type": "Message",
  "event_id": "...",
  "content": {"role": "assistant", "content": "Hello back!"},
  "timestamp": "2026-04-01T12:00:00Z"
}
```

History is replayed from ClickHouse on connect, and live events are tailed from Pulsar, so you can subscribe late or reconnect with the `Last-Event-ID` header without losing events.

### Multi-turn conversations

The caller owns conversation state: keep your own `session_id` and pass prior turns in `payload.history`. The agent receives the full payload on stdin:

```sh
curl -X POST "http://<gateway>:3000/run" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $DIMENSION_TOKEN" \
  -d '{
    "bundle_id": "my-agent",
    "mode": "async",
    "payload": {
      "role": "user",
      "content": [{"type": "text", "text": "Follow-up question"}],
      "session_id": "<same session uuid>",
      "history": [
        {"role": "user", "content": "Previous message", "timestamp": "..."},
        {"role": "assistant", "content": "Previous response", "timestamp": "..."}
      ]
    }
  }'
```

---

## Using the Agent SDK (Optional)

The `@dimension-agents/shared` workspace package (in `agents/packages/shared`) provides the agent harness and typed access to the outbound event stream. Outbound messages travel over a Unix socket (`DIMENSION_EVENTS_SOCK`) bridged by `dimension-agent` to the host, where they surface as `bundle` SSE events.

### Usage

```typescript
import { runAgent, dimension, bashTool, readFileTool } from "@dimension-agents/shared";

// Full agent lifecycle: parse stdin payload → run LLM agent loop with tools
// → write final response to stdout.
await runAgent({
  systemPrompt: "You are a helpful agent.",
  tools: [bashTool, readFileTool],
});
```

For progress and results mid-run:

```typescript
import { dimension, sendMessage } from "@dimension-agents/shared";

// Bridge-consumable message (forwarded to the end user by bridges)
await sendMessage("Working on it…");

// Arbitrary structured event (kind defaults to "bundle")
await dimension.send({ body: { step: "compile", status: "ok" } });
```

Outside a Dimension VM (env var unset) all sends are no-ops, so the same code runs locally for development.

### Input format

Your agent receives a JSON object on stdin:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "Hello, agent!"}
  ],
  "session_id": "session-uuid",
  "bundle_id": "my-agent",
  "history": [
    {"role": "user", "content": "Previous message", "timestamp": "..."},
    {"role": "assistant", "content": "Previous response", "timestamp": "..."}
  ]
}
```

Parse `content` to get the user's message. `history` contains prior turns for multi-turn conversations.

---

## Advanced: A2A Agent Discovery

If your `dimension.toml` includes an `[a2a]` section, the gateway auto-generates an A2A-compatible agent card:

```
GET /agents/my-agent/
```

Other agents (inside or outside Dimension) can discover and call your agent using the standard A2A protocol. This is how agent-to-agent communication works — one agent calls another by name.

---

## Advanced: Persistent Volumes

Bundles with `[volumes]` get a persistent ext4 volume at `/workspace`:

```toml
[volumes]
size = "10GB"
```

The volume persists across sessions. Use it for:
- Caching downloaded models or data
- Maintaining state between conversations  
- Storing intermediate work products

Without `[volumes]`, the filesystem is ephemeral — everything is gone when the VM shuts down.

### Named volumes

For shared data across sessions or bundles:

```toml
[volumes]
shared_mount = "my-shared-volume"
```

Named volumes use advisory locking — only one VM can mount a named volume at a time (returns 409 if already in use).

---

## Troubleshooting

### Upload fails with "missing required field: image"

The multipart form field must be named `image`:
```sh
# Correct
curl -F "image=@my-agent.tar" ...

# Wrong
curl -F "file=@my-agent.tar" ...
```

### Upload fails with "archive validation failed"

The tar must be a `docker save` output, not `docker export`. Check for:
- Symlinks (rejected for security)
- Files larger than 4 GiB
- Total extracted size over 8 GiB

### Agent runs but produces no output

- Check that your entrypoint.sh uses `exec` (not just running the command)
- Make sure `/etc/hyphae/env` is sourced (secrets and env vars live there)
- Verify your agent reads from stdin and writes to stdout
- If using the SDK, check that `init()` succeeded — look for `[dimension-sdk]` errors in stderr

### "invalid dimension.toml" error

The manifest parser is strict — unknown fields cause errors. Check for:
- Typos in section names (e.g., `[capabilites]` instead of `[capabilities]`)
- Unknown fields within sections
- Invalid TOML syntax

### Timeout — VM killed

Increase `timeout_secs` in `dimension.toml`:
```toml
[resources]
timeout_secs = 600  # 10 minutes
```

The server has a maximum timeout cap — check with your operator if large values are rejected.

### Network errors inside the VM

Make sure your Dockerfile includes the DNS setup:
```dockerfile
RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm
```

And your entrypoint.sh copies it:
```sh
if [ -f /etc/resolv.conf.vm ]; then
    cp /etc/resolv.conf.vm /etc/resolv.conf 2>/dev/null || true
fi
```

---

## Reference: Full dimension.toml Schema

```toml
# All sections are optional. Missing sections use safe defaults.

[resources]
memory_mb = 256          # integer, default: 256
vcpus = 2                # integer (1-8), default: 2
timeout_secs = 120       # integer, default: server-configured

[capabilities]
storage = false          # bool, default: false
agent_calls = false      # bool, default: false
secrets = false          # bool, default: false
tokenize = false         # bool, default: false

[env]
# Flat key = "value" pairs. Injected as env vars at boot.
# Applied AFTER Docker ENV, so these override image defaults.
MY_VAR = "value"

[secrets]
names = ["SECRET_NAME"]  # string array. Values injected at runtime.

[volumes]
size = "10GB"            # string. Creates a persistent ext4 volume.
# shared_mount = "name"  # string. Mount a named volume instead.

[a2a]
name = "agent-name"      # string. Agent name in the A2A registry.
description = "..."      # string. Human-readable description.
skills = ["skill1"]      # string array. Advertised capabilities.
```

---

## Reference: API Endpoints

### Bundle management

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/bundles/upload?name=X&tag=Y` | Bearer | Upload a Docker image tar |
| `GET` | `/bundles/jobs/{id}` | Bearer | Poll upload job status |
| `GET` | `/bundles` | Bearer | List your bundles + platform bundles |

### Running

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/run` | Bearer | Run a bundle (`mode`: `sync` or `async`) |
| `GET` | `/runs/{id}/events` | Bearer | SSE stream of run events (`state` + `bundle`) |
| `POST` | `/run/{id}/stop` | Bearer | Stop a running invocation |
| `GET` | `/invocations/{id}` | Bearer | Invocation status |

### Admin (requires admin role)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/admin/users` | Create a user |
| `GET` | `/admin/users` | List users |
| `POST` | `/admin/users/{id}/keys` | Create an API key for a user |
| `GET` | `/admin/bundles` | List all bundles |
| `DELETE` | `/admin/bundles/{id}` | Delete a bundle |
| `POST` | `/admin/bundles/{id}/rebuild` | Rebuild a bundle |
