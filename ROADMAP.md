# Dimension Roadmap

Future work and active backlog. Completed milestones (v1.0, v1.1) are tracked in `.planning/`. Consolidated task list in `TODO.md`.

---

## Future Features

### Tool Call & Response Persistence (Partially Implemented)

**Status:** Session events table exists (migration 0012). The dispatch layer persists `tool_call` and `tool_result` event types via `session_events`. The webhook system delivers these events. The remaining gap is agent-side: the agent SDK's tool execution loop doesn't yet POST each tool call/result to the session events API during execution — only the final response is captured.

**Remaining work:**
- Hook into the agent's tool execution in `shared/src/agent.ts` to emit tool call/result events via the SDK's `sessions.appendMessage()` during execution (not just the final response)

### Reusable Agent Skills

- Skills bundle tools + prompt fragment + config into composable units
- Agents assembled from skills by name
- Stored in registry, versioned
- Resolved at launch time (tools + prompt concatenated)
- Skills can depend on other skills
- Meta-agent picks skills when building new agents

### Artifact Management (Partially Implemented)

**Status:** Artifacts API exists (migration 0011, `handlers/artifacts.rs`). Publish, list, get, and delete endpoints are functional. TTL support added (migration 0015). The remaining gap is the split between the `storage.*` API and the `artifacts.*` API — see TODO.md "Artifacts Overhaul" for the unification plan.

- Agents upload artifacts via SDK (`dimension.artifacts.upload`)
- Stored in S3-compatible storage (Garage/MinIO), metadata in Postgres
- Persist beyond VM teardown

### Registry

- Central catalog of tools, agents, and skills
- Queryable at runtime — agents discover available capabilities
- Backed by Postgres
- Meta-agent uses registry to assemble new agents

### Meta-Agent

- Built-in agent whose job is to build and deploy other agents
- Selects tools/skills from registry, writes prompts, deploys
- Tests with promptfoo before deploying
- Agent factory running inside Dimension itself

### Parameterized Agent Runtime

- Generic rootfs with shared tools + agent harness, no prompt baked in
- At launch: Dimension injects prompt, tool list, capabilities, model config
- Deploy agents as config, not code — zero build time
- Custom agents (needing Chromium, etc.) still use dedicated rootfs
- Simple agents use parameterized runtime

### Guest-Side VM Lifecycle

- Move health monitoring from host-side reaper into dimension-agent (guest side)
- Agent monitors child process, sends error/timeout frames over vsock
- Clean shutdown via hyphae-init reboot
- Per-bundle timeout config via manifest
- Host-side reaper already disabled (had race condition)

### promptfoo Integration

- Each agent package has `promptfoo.yaml` with test cases
- `dimension-cli sessions export-promptfoo` exports real conversations as tests
- Future: define agent FROM promptfoo yaml (reverse generation)
- Meta-agent uses promptfoo to validate before deploying

### Per-Bundle VPN / Network Isolation

- Bundles that need access to private servers behind VPNs get isolated WireGuard tunnels
- Bundle manifest declares VPN config: `network.vpn = "vault:vpn/customer-a-wireguard"`
- At VM launch, gateway resolves WireGuard config from Vault and injects into rootfs
- hyphae-init brings up `wg0` interface inside the VM before starting the agent
- Each VM gets its own isolated tunnel — other bundles can't see or use it
- VM teardown destroys the tunnel — no cleanup needed
- Multi-tenant safe: VPN access is scoped per-bundle, not per-host

### Long-Running VMs & Agent-Provisioned Compute

- Agents can request persistent compute resources that outlive a single message turn
- Use case: an agent building an app needs a database, web server, dev environment
- `dimension.compute.create({ vcpus, memory_mib, image, ports })` via SDK
- Long-running VMs persist across turns/sessions, destroyed explicitly or by TTL
- Per-user/per-task resource quotas, billing/metering
- Stable IPs on TAP network, optional external port exposure

### Worker Auto-Registration on Boot (Implemented)

Workers register with the gateway via `POST /internal/workers/register` on startup with retry and exponential backoff. The gateway health-polls workers and auto-removes after 3 consecutive misses. See DEPLOY-GUIDE.md for details.

---

## Backlog (Bug Fixes)

- [ ] Gateway restart disconnects all workers — workers lose gRPC connection on gateway restart and must re-register. In-flight VMs are lost. Need graceful drain or worker-side auto-reconnect with backoff.
- [ ] Env vars not visible to agent tools — hyphae-init passes runtime env vars via kernel cmdline to dimension-agent, which inherits them to child processes. But agent tool subprocesses may not see them if the agent framework doesn't propagate them. Need to verify pi-agent-core tool execution inherits the full process environment.
- [ ] VM rootfs disk space too small — coder agent runs out of space when installing large packages (e.g. Next.js). Need configurable rootfs size or overlay filesystem for workspace.
- [ ] VM runtime directory collision — "failed to create VM runtime directory: Not a directory (os error 20)". /tmp/hyphae/vms path conflicts, likely a file exists where a directory is expected. Need cleanup or unique runtime dir paths per VM.
- [ ] User-facing artifact browsing — artifacts are now session-scoped (`/{user_id}/{bundle_id}/{session_id}/`) but `GET /bundles/{id}/storage` still uses the old non-session path. Need either recursive listing across sessions or a new endpoint `GET /bundles/{id}/sessions/{session_id}/storage` with optional session filter.
- [ ] Kaibigan: implement `selectAppointmentTime`
- [ ] Kaibigan: browser automation timeout handling

---

_Last updated: 2026-04-01_
