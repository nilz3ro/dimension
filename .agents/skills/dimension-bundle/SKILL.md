---
name: dimension-bundle
description: Create, inspect, or fix Dimension bundles whose entrypoint reads the invocation payload directly from stdin. Use when building a Dimension bundle without a shared agent harness, when explaining the Dimension payload contract, when writing Dockerfiles or simple shell/Python/Rust entrypoints for Dimension, or when testing `dimension run --payload` delivery.
---

# Dimension Bundle

## Contract

Treat Dimension as a compute launcher. The bundle entrypoint receives the invocation payload as raw bytes on stdin. The payload is normally JSON, but the platform does not require any shared agent harness.

Require the deployed bundle to include the Dimension sidecar. In this repo, `dimension build` embeds it by default; `hyphae build` needs `--with-dimension`. Without `/sbin/dimension-agent`, `hyphae-init` runs the entrypoint directly and gateway payload delivery is unavailable.

## Minimal Bundle

Prefer the smallest runtime that satisfies the task. Use shell or Python for examples unless the user asks for another language. Do not add a language-specific SDK, package manager, UI stack, or shared agent library unless the user explicitly asks for one.

Create an entrypoint that:

1. Reads all of stdin until EOF.
2. Validates or parses JSON if the bundle expects JSON.
3. Writes the final result to stdout.
4. Writes diagnostics to stderr.
5. Exits nonzero on invalid payloads or execution failure.

Example POSIX shell entrypoint:

```sh
#!/bin/sh
set -eu

payload="$(cat)"

if [ -z "$payload" ]; then
  echo "empty stdin payload" >&2
  exit 1
fi

printf 'payload=%s\n' "$payload"
```

Example Python entrypoint:

```python
#!/usr/bin/env python3
import json
import sys

raw = sys.stdin.read()
if not raw:
    print("empty stdin payload", file=sys.stderr)
    sys.exit(1)

try:
    payload = json.loads(raw)
except json.JSONDecodeError as exc:
    print(f"invalid JSON payload: {exc}", file=sys.stderr)
    sys.exit(1)

print(json.dumps({"received": payload}, separators=(",", ":")))
```

Example Dockerfile:

```Dockerfile
FROM python:3.12-slim

WORKDIR /app
COPY entrypoint.py /app/entrypoint.py
RUN chmod +x /app/entrypoint.py

ENTRYPOINT ["/app/entrypoint.py"]
```

Optional `dimension.toml`:

```toml
[resources]
memory_mb = 256
vcpus = 1
timeout_secs = 60
```

## Build And Run

Build and deploy from a Dockerfile:

```sh
dimension build -f Dockerfile --context . --name stdin-echo
```

Run with the CLI's default message wrapper:

```sh
dimension run stdin-echo "hello"
```

Run with a raw JSON payload:

```sh
dimension run stdin-echo --payload '{"value":"hello"}'
```

Run with a raw JSON payload from a file:

```sh
dimension run stdin-echo --payload @payload.json
```

Use sync mode while debugging so stdout is returned directly. Use async mode only when the bundle has its own callback or persistence path.

## Troubleshooting

If the entrypoint receives no stdin, check that the bundle was built with the Dimension sidecar and deployed through the gateway path.

If the invocation hangs before the entrypoint logs anything useful, check whether the program reads stdin. The sidecar writes the full payload into the entrypoint's stdin pipe and then closes it; a large payload can block if the child process never reads.

If stdout is empty, confirm the program writes the intended response to stdout, not stderr. Dimension treats stdout as the invocation result path.

If JSON parsing fails, test locally first:

```sh
printf '{"value":"hello"}' | ./entrypoint.py
```

Then test with Dimension using `--payload` to bypass the CLI's default message envelope.
