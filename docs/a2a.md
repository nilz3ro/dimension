# A2A & Inter-Agent Communication

## Architecture Decision: Two Paths

### External A2A (cross-platform, federation)
For agents on OTHER platforms calling into Dimension, or external clients (CI, apps, scripts).

```
External caller → POST /agents/{name}/ (JSON-RPC, A2A protocol)
  → gateway validates request
  → gateway dispatches to worker → VM runs → collects response
  → returns A2A JSON-RPC response
```

Standard A2A protocol. Streaming optional (push notifications or polling).

### Internal dispatch (Dimension agents talking to each other)
For agents INSIDE Dimension calling other Dimension agents.

```
Coder VM → SDK agents.send("planner", msg)
  → localhost:8765 (vsock bridge)
  → proxy /v1/agents/send
  → gateway.dispatch_internal(target_bundle, message, user_id, caller_session)
    → orchestration handler runs target VM directly
    → collects full text response (no SSE, no HTTP self-call)
  → returns {"response": "..."}
```

No A2A protocol. No SSE streaming. No gateway-calling-itself. Direct dispatch.

## Why NOT use A2A internally

The current broken flow:
```
proxy /v1/agents/send handler
  → reqwest POST http://127.0.0.1:3000/messages  ← gateway HTTP-calls ITSELF
  → gateway spawns target VM, streams SSE back TO ITSELF
  → handler parses its own SSE stream to extract text  ← lossy, fragile
  → returns {"response": "extracted text"}
```

Problems:
1. **Gateway calls its own HTTP endpoint** — unnecessary network round-trip to self
2. **SSE is a client streaming format** — exists for Telegram/web UIs that need incremental updates. Agent-to-agent calls want the final answer, not a stream of chunks
3. **Response extraction is lossy** — `extract_response_from_sse()` concatenates `content` fields from message events, losing structure, tool calls, metadata
4. **Error handling is broken** — if the nested VM errors, the SSE stream may have no message events, returning empty string instead of error
5. **Auth complexity** — handler needs a `gateway_bearer_token` to call itself, which is circular

## Internal dispatch design

### `send_agent_handler` (proxy route, stays)
The SDK calls `POST /v1/agents/send`. But instead of HTTP-calling `/messages`, the handler:

1. Validates auth (JWT caps.agents = true)
2. Gets/creates inter-agent session
3. Calls `orchestration_handler.execute_request()` directly (in-process)
4. Waits for VM completion
5. Returns full text response

### What `execute_request` already does
The orchestration handler already:
- Resolves the bundle from registry
- Launches a Firecracker VM on a worker
- Connects via vsock
- Sends the request and waits for the full response
- Returns the response text

The `send_agent_handler` just needs to call this directly instead of going through HTTP.

### Call-depth guard (stays)
Still enforce one-hop limit. The nested VM's JWT has `caps.agents = false` so it can't make further agent calls. This prevents recursive chains.

### Session management (stays)
Inter-agent sessions are still persistent — caller→target pair reuses the same session across calls. This gives the target agent conversation history with the caller.

## External A2A (future, separate)

External A2A is a DIFFERENT code path:
- `POST /agents/{name}/` (JSON-RPC endpoint, per A2A spec)
- Accepts `message/send`, `message/get`, `tasks/get` methods
- Maps to internal dispatch (same orchestration handler)
- Returns A2A-formatted response (artifacts, status, contextId)
- Optional: streaming via SSE or push notifications

External A2A wraps the internal dispatch with protocol translation. Internal dispatch is the primitive.

## Current status

### Working ✅
- vsock platform channel (VM ↔ gateway without network)
- `a2a_discover` (agents.list via proxy)
- Agent cards at `/.well-known/agent.json`
- JWT auth and capability gating
- Call-depth guard (one-hop limit)

### Broken ❌  
- `a2a_send` returns empty/error because it HTTP-calls /messages and parses SSE (the wrong approach)
- vsock UDS files not appearing on workers (Firecracker vsock config may not be applied — needs debug)

### TODO
1. **Replace HTTP self-call with direct dispatch** in `send_agent_handler`
   - File: `crates/dimension-gateway/src/proxy/handlers.rs`
   - The handler needs access to `VmOrchestrationHandler` (add to `ProxyState`)
   - Call `handler.execute_request()` directly
   - Remove `extract_response_from_sse()`, `gateway_bearer_token`, reqwest dependency

2. **Debug vsock on workers**
   - Verify hyphae binary on workers supports vsock config
   - Check if `launch()` applies vsock to Firecracker API
   - Look for `v.sock` files in VM runtime dirs during VM execution

3. **External A2A endpoint** (future, separate work)
   - `POST /agents/{name}/` JSON-RPC handler
   - Protocol translation layer: A2A ↔ internal dispatch

## Key files

- `crates/dimension-gateway/src/proxy/handlers.rs` — `send_agent_handler` (needs rewrite)
- `crates/dimension-gateway/src/proxy/server.rs` — `ProxyState` (needs orchestration handler)
- `crates/dimension-gateway/src/orchestration/handler.rs` — `execute_request` (the primitive)
- `crates/dimension-gateway/src/orchestration/vsock_proxy.rs` — host-side vsock proxy
- `crates/hyphae-init/src/main.rs` — guest-side vsock bridge
- `agents/packages/sdk/src/agents.ts` — SDK `agents.send()`, `agents.list()`
- `agents/packages/shared/src/tools/a2a.ts` — tool wrappers
