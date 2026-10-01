# Buzz chat agent

Tool-free Buzz chat on the unchanged `@dimension-agents/shared`
`runAgent` harness. The launcher accepts either payload:

```json
{"session_id":"buzz-thread","message_text":"Hello"}
```

```json
{"session_id":"dimension-session","content":[{"type":"text","text":"Hello"}]}
```

An optional `history` array carries the prior session turns, oldest first:

```json
{"session_id":"buzz-thread","message_text":"What was the code word?","history":[{"role":"user","content":"the code word is bluebird","timestamp":"2026-10-01T10:00:00Z"},{"role":"assistant","content":"noted","timestamp":"2026-10-01T10:00:05Z"}]}
```

The bridge owns bounding and persistence of that history; the bundle only
validates it (user/assistant text turns, at most 200 entries) and forwards it
to the model. It sets `CONVERSATION_ID` from `session_id`,
and pipes normalized Dimension input into the harness. A successful turn writes
only final assistant text to stdout, consumed by Dimension as `stdout-final`.
Errors go to stderr with nonzero exit status; empty, truncated, tool-request,
redirect, HTTP-error and malformed model replies fail closed.

## Model configuration

Both `MODEL_BASE_URL` and `MODEL_NAME` are required at process startup. The
manifest configures the SATHQ endpoint `http://192.168.105.168:8000/v1` and
`muse-glimmer-30b`. Phiveman identified this service as llama.cpp; it speaks
OpenAI-compatible chat completions and needs no authentication. Runtime manifest
environment can override these values without changing the transport.

The package-local Pi API extension makes one non-streaming `/chat/completions`
request, sends no tools or Authorization header, ignores ambient API keys, and
never falls back to another provider. This is necessary because the installed
Pi built-in OpenAI transport requires an API key even for a no-auth endpoint.
Requests have a 210-second deadline within the manifest's 240-second VM limit;
maximum output is 2048 tokens. No persistent volumes or capabilities are requested.

## Verify and build

From `agents/`:

```sh
pnpm install --frozen-lockfile
pnpm --filter @dimension-agents/buzz-agent test
pnpm -r check
docker build --platform linux/amd64 -f packages/buzz-agent/Dockerfile -t dimension-buzz-agent .
```

The Dockerfile-specific ignore file excludes host dependencies and build output.
The image skips Puppeteer's browser download; the shared package's tools are
not passed to the agent. Tests exercise the actual launcher and shared harness
against a local mock endpoint for both payload shapes, including failure paths
and absence of credentials on the wire. History forwarding is covered too:
the mock endpoint sees the validated history messages ahead of the new user
turn. They do not prove SATHQ reachability,
Firecracker networking, bundle upload, or a live Buzz reply.

For the live milestone, Phiveman builds/uploads/deploys the reviewed commit and
verifies guest reachability after the committed destination allowlist lands.
Nihao then verifies one threaded, deduplicated model-backed Buzz reply.
See [`docs/BUNDLES.md`](../../../docs/BUNDLES.md) for the upload contract.
