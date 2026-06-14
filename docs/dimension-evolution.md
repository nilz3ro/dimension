# Dimension Evolution: Stateful Agent Runtime

## Current Architecture

```
Message In (HTTP/Telegram)
  → Auth + Validation
  → Spawn Firecracker VM
  → Connect over vsock
  → Forward opaque JSON to guest agent
  → Guest agent spawns user binary (which calls LLM, runs tools, etc.)
  → Stream stdout/stderr back as SSE
  → VM teardown
```

Today, Dimension is a **stateless gateway**. It manages VM lifecycle and streams
responses, but has no awareness of what happens inside the VM. The request payload
is opaque JSON. There is no persistence between requests — each message gets a
fresh VM that is destroyed after the response.

### What exists today
- HTTP API (POST /messages, GET /health) with SSE streaming
- Firecracker VM orchestration with lifecycle guards
- Protobuf wire protocol over vsock (varint-framed envelopes)
- Guest agent that spawns arbitrary binaries and streams their output
- Telegram channel integration (polling mode)
- Bundle registry (SQLite) for VM image management
- Concurrency control, auth, graceful shutdown, orphan reaping

---

## Proposed Architecture

### Core Insight

The VM should remain the containment/execution boundary (kill switch for rogue
agents), but Dimension should become a **stateful runtime** that manages the
continuity between ephemeral VM invocations.

### Proposed Flow

```
Message In
  → Resolve session (load history, context, filesystem refs)
  → Spawn VM with session filesystem mounted
  → Connect over vsock
  → Forward message + conversation history to agent
  → Agent runs autonomously (calls LLM, uses tools, writes files)
  → Stream response back as SSE
  → Persist session state (conversation history, filesystem changes)
  → VM teardown
```

The agent remains a self-contained program inside the VM — it calls the LLM
provider, executes tools, and manages its own agentic loop. Dimension doesn't
intercept or proxy LLM calls. What changes is that Dimension provides
**persistent context** to each ephemeral VM: conversation history, filesystem
state, and artifacts from prior invocations.

The VM is still the containment boundary. Dimension can kill rogue agents.
But now each VM boots into a world that "remembers" what came before.

---

## New Components

### 1. Session Store

Manages the persistent state of a user's interaction across multiple requests.

```
Session {
  id: SessionId,
  user_id: UserId,
  channel: ChannelInfo,           // telegram, slack, http, etc.
  conversation_history: Vec<Message>,
  filesystem_id: Option<FilesystemId>,
  artifacts: Vec<ArtifactRef>,
  created_at: Timestamp,
  last_active: Timestamp,
  metadata: HashMap<String, Value>,
}
```

**Responsibilities:**
- Map (user, channel) → session
- Store and retrieve conversation history
- Track which filesystem volumes belong to this session
- Track artifacts produced by agents

**Storage backend:** Pluggable — start with SQLite for local dev, design the
trait so it can back onto Postgres, DynamoDB, etc.

### 2. Object Storage

Persistent storage for files and artifacts produced by agents.

```
ObjectStore trait {
  put(key, data, metadata) → ObjectRef
  get(key) → data
  list(prefix) → Vec<ObjectRef>
  delete(key)
}
```

**What gets stored:**
- Files the agent creates or modifies
- Conversation context snapshots
- Tool outputs / intermediate results
- User-uploaded files

**Backend:** Pluggable — local filesystem for dev, S3-compatible for production.

### 3. Filesystem Mount Layer

Presents stored state as a filesystem to the VM at startup.

```
Volume {
  id: VolumeId,
  session_id: SessionId,
  mount_point: String,           // e.g. "/workspace"
  mode: ReadOnly | ReadWrite,
  snapshot_id: Option<SnapshotId>,
}
```

**Lifecycle:**
1. Before VM boot: prepare filesystem image from object store
2. Mount as additional drive in Firecracker config
3. Agent sees `/workspace` (or configured mount point) with prior state
4. On VM teardown: diff the filesystem, persist changes back to object store
5. Snapshot for next invocation

**Considerations:**
- Copy-on-write / overlay FS to minimize snapshot size
- Read-only base layers (shared tools, libraries) vs read-write session layer
- Multiple volumes per VM (e.g., `/workspace` RW + `/shared-tools` RO)

### 4. Context Injection

Before the agent starts, Dimension assembles the request payload with context:

- Conversation history (from session store)
- System prompt / agent configuration
- Filesystem state summary (what files exist, recent changes)
- User profile / preferences
- Channel-specific context (thread info, mentions, etc.)

This enriched payload is what gets forwarded to the guest agent over vsock.
The agent is a self-contained program — it receives the full context and calls
the LLM provider directly. Dimension doesn't proxy or intercept LLM calls.

---

## Architecture Diagram

```
                    ┌──────────────────────────────────────────┐
                    │               DIMENSION                   │
                    │                                           │
  Channels ────────►│  ┌───────────┐  ┌───────────────────┐   │
  (Telegram,        │  │  Session   │  │   Orchestration    │   │
   Slack, HTTP)     │  │  Store     │  │   (VM lifecycle,   │   │
                    │  │            │  │    context inject)  │   │
                    │  └─────┬──────┘  └─────────┬─────────┘   │
                    │        │                    │              │
                    │  ┌─────┴──────┐             │              │
                    │  │  Object    │             │              │
                    │  │  Store     │             │              │
                    │  └─────┬──────┘             │              │
                    │        │                    │              │
                    │  ┌─────┴────────────────────┴──────────┐  │
                    │  │       Filesystem Mount Layer          │  │
                    │  └─────┬───────────────────┬────────────┘  │
                    └────────┼───────────────────┼───────────────┘
                             │                   │
                        ┌────┴────┐         ┌────┴────┐
                        │  VM     │         │  VM     │
                        │  Agent  │         │  Agent  │
                        │ (calls  │         │ (calls  │
                        │  LLM,   │         │  LLM,   │
                        │  tools) │         │  tools) │
                        │ /workspace ◄──mounted──► object store
                        └─────────┘         └─────────┘
```

---

## Migration Path

### Phase 1: Session Store
- Add session store trait + SQLite implementation
- Map (user, channel) → session with conversation history
- Wire into existing message flow — store history, inject context
- No changes to VM lifecycle yet

### Phase 2: Object Storage + Filesystem Layer
- Add object store trait + local filesystem implementation
- Implement volume creation/mounting in Firecracker config
- Persist filesystem state between invocations
- Add snapshot/diff mechanism for efficient storage

### Phase 3: Context Injection
- Enrich request payload with conversation history before forwarding to agent
- Include filesystem manifest (what files exist from prior runs)
- Channel-specific context (thread info, user profile)

### Phase 4: Multi-Session + Multi-Channel
- Multiple concurrent sessions per user
- Cross-channel session continuity (start on Telegram, continue on HTTP)
- Session lifecycle management (TTL, quotas, cleanup)

---

## Open Questions

- **VM warm pools**: Keep idle VMs around for instant tool execution, or always
  cold-start? Cold start is simpler but adds latency per tool call.
- **Filesystem granularity**: Full disk snapshots vs file-level object storage?
  Snapshots are simpler but wasteful. File-level is efficient but complex.
- **Multi-agent sessions**: Can a session have multiple agents collaborating?
  e.g., a coding agent and a review agent sharing the same workspace.
- **Session lifecycle**: When does a session expire? Manual cleanup? TTL-based?
  Storage quotas per user?
