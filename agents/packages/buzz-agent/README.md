# Buzz chat agent

Tool-free, stateless Buzz chat on the unchanged `@dimension-agents/shared`
`runAgent` harness. The launcher accepts either payload:

```json
{"session_id":"buzz-thread","message_text":"Hello"}
```

```json
{"session_id":"dimension-session","content":[{"type":"text","text":"Hello"}]}
```

It sets `CONVERSATION_ID` from `session_id`, discards caller-supplied history,
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
and absence of credentials on the wire. They do not prove SATHQ reachability,
Firecracker networking, bundle upload, or a live Buzz reply.

For the live milestone, Phiveman builds/uploads/deploys the reviewed commit and
verifies guest reachability after the committed destination allowlist lands.
Nihao then verifies one threaded, deduplicated model-backed Buzz reply.
See [`docs/BUNDLES.md`](../../../docs/BUNDLES.md) for the upload contract.
