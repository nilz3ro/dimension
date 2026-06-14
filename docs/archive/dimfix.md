# Dimension Build/Push Fix Plan

## Goal

Make `dimension build` + `dimension push` the primary, portable workflow for bundle creation and deployment.

Desired user experience:

```bash
dimension build ... --output bundle.ext4
dimension push --name my-bundle --tag latest --file bundle.ext4
```

Users should **not** need to run `docker build`, `docker save`, or call `/bundles/upload` manually.

The output of `dimension build` should always be a **ready-to-run ext4 bundle** that Dimension can consume immediately via `/bundles/push`.

---

## Current Problems

### 1. `dimension build` is not portable across host OSes

Today, the build path depends on host-native Linux tooling and Linux binaries:

- embedded `hyphae-init`
- embedded `dimension-agent`
- embedded Linux Node binary for JS projects
- `mkfs.ext4`
- Linux-oriented rootfs assembly

On non-Linux hosts, especially macOS:

- `hyphae-core/build.rs` writes placeholder binaries instead of real Linux ELF binaries
- `validate_init()` later rejects those placeholders because they are not valid ELF files
- `mkfs.ext4` may not exist in `PATH`

Result: `dimension build` fails before it can produce a valid ext4.

### 2. The current build model is too language-specific

The old Hyphae behavior tries to infer bundle type from source layout:

- `package.json` => JavaScript
- `Cargo.toml` => Rust
- otherwise fail

That is too restrictive.

A bundle should not depend on the CLI understanding the source language. Any workload that can run inside a Linux filesystem should be supported, including:

- Python
- Ruby
- Go
- Bash
- Java
- mixed/polyglot repos
- custom runtimes

### 3. Manual Docker tar upload is the wrong primary UX

`/bundles/upload` is useful as a compatibility path, but it should not be the main developer workflow.

The desired platform model is:

- **canonical artifact**: ext4
- **canonical deploy step**: `dimension push`
- **canonical build command**: `dimension build`

---

## Product Direction

### Canonical workflow

The intended workflow should be:

```bash
dimension build --dockerfile <path> --context <dir> --output bundle.ext4
dimension push --name <bundle> --tag <tag> --file bundle.ext4
```

Optional convenience later:

```bash
dimension build --dockerfile <path> --context <dir> --push --name <bundle> --tag <tag>
```

### Canonical build input

`dimension build` should be based on an **explicit runtime image definition**, not source-language auto-detection.

Preferred inputs:

1. **Dockerfile + build context**
2. **Existing Docker/OCI image reference**

Possible escape hatches later:

3. explicit rootfs directory input
4. explicit binary/directory wrapping input

But these should be opt-in and explicit, not auto-detected.

---

## Proposed Design

## 1. Keep ext4 as the canonical bundle artifact

Do not change the deploy side.

- `dimension push` remains the upload path for ready-to-run ext4 bundles
- `/bundles/push` remains the primary ingestion endpoint
- `/bundles/upload` remains a compatibility / alternate path, not the default UX

## 2. Make `dimension build` backend-driven

Add a builder backend concept:

```bash
dimension build --builder auto
dimension build --builder native
dimension build --builder docker
```

Recommended behavior:

- `auto` = default
- use `native` on Linux when prerequisites are available
- use `docker` on macOS/Windows or whenever native prerequisites are missing

This preserves one CLI while allowing portable execution.

## 3. Use Docker as an internal Linux build backend

When `--builder docker` is selected, or `auto` resolves to Docker:

- the host CLI launches a Linux builder container
- the Linux builder performs the actual rootfs assembly
- the container writes a real ext4 back to the host output path

From the user's perspective, it is still just:

```bash
dimension build ...
```

Docker is only an implementation detail.

## 4. Introduce a dedicated builder image

Use a dedicated versioned image, e.g.:

```text
ghcr.io/zeropoint/dimension-builder:<version>
```

This image should contain:

- Linux userspace
- `mkfs.ext4` / `e2fsprogs`
- real `hyphae-init`
- real `dimension-agent`
- any embedded runtime assets needed by the rootfs builder
- cargo + required Linux targets if source-side compilation is still needed internally
- any other deterministic build dependencies

The purpose of this image is to provide a stable Linux environment for bundle construction.

## 5. Re-exec build logic inside the builder container

Do not maintain two different rootfs assembly implementations.

Instead, the host CLI should invoke a Linux-side internal build command inside the container, conceptually like:

```bash
docker run --rm \
  --platform linux/amd64 \
  -v <src>:/src \
  -v <out>:/out \
  ghcr.io/zeropoint/dimension-builder:<version> \
  dimension __internal_build ...
```

This keeps the build logic unified while making it portable.

---

## Language Model Change

## Remove automatic JS/Rust project detection from the primary build path

The following should no longer define the default build UX:

- automatic detection via `package.json`
- automatic detection via `Cargo.toml`
- JS-specific source packaging as the main entry path
- Rust-specific source compilation as the main entry path

In other words:

> Dimension should build bundles from explicit runtime images, not inferred source trees.

## Replace language inference with explicit image-oriented inputs

Recommended public CLI shape:

```bash
dimension build --dockerfile <path> --context <dir> --output bundle.ext4
dimension build --image <image-ref> --output bundle.ext4
```

This makes `dimension build` work for anything Docker can build, including Python and Ruby agents, without adding language-specific logic to Dimension.

---

## Expected Build Flow

### A. Dockerfile path

```bash
dimension build --dockerfile agents/packages/coder/Dockerfile --context agents --output coder.ext4
```

Internal flow:

1. build a Linux image from the Dockerfile
2. inspect/export the image filesystem
3. inject/overwrite Dimension runtime files as needed
4. read `/etc/hyphae/dimension.toml`
5. assemble ext4
6. write `coder.ext4`

### B. Existing image path

```bash
dimension build --image dimension-coder:latest --output coder.ext4
```

Internal flow:

1. inspect/export the image filesystem
2. inject/overwrite Dimension runtime files as needed
3. assemble ext4
4. write output

---

## Architecture Handling

Architecture must be explicit and must not accidentally follow host architecture.

Suggested flag:

```bash
dimension build --arch x86_64
```

Default recommendation for now:

- default target architecture: `x86_64`

When using Docker backend:

- build images with `--platform linux/amd64`
- ensure embedded runtime binaries match the selected guest architecture
- do not let Apple Silicon default to `linux/arm64` unless explicitly requested

Later, add `aarch64` support deliberately rather than implicitly.

---

## Why This Is Better

This design gives:

- one consistent workflow across Linux and macOS
- ext4 as the canonical artifact
- immediate consumption by Dimension via `push`
- no requirement for users to manually run Docker commands
- no language-specific bundle logic in the CLI
- support for any runtime that can be expressed as a Docker image
- cleaner documentation and better long-term UX

---

## CLI Proposal

### Primary commands

```bash
dimension build --dockerfile <path> --context <dir> --output bundle.ext4
dimension build --image <image-ref> --output bundle.ext4
dimension push --name <bundle> --tag <tag> --file bundle.ext4
```

### Backend controls

```bash
dimension build --builder auto
dimension build --builder native
dimension build --builder docker
```

### Architecture control

```bash
dimension build --arch x86_64
```

### Future convenience

```bash
dimension build --dockerfile <path> --context <dir> --push --name <bundle> --tag <tag>
```

---

## Implementation Plan

### Phase 1: Define the new public contract

- make Dockerfile/image-based build the primary documented path
- de-emphasize or deprecate source-language auto-detection
- document ext4 + push as the canonical flow

### Phase 2: Add backend selection to `dimension build`

- add `--builder auto|native|docker`
- make `auto` default
- implement backend decision rules

### Phase 3: Create the Linux builder image

- produce a versioned builder image
- include all Linux-only build dependencies
- make it deterministic and CI-publishable

### Phase 4: Add Docker-backed build execution

- host CLI launches builder container
- mount inputs/outputs appropriately
- invoke internal Linux build logic inside the container
- ensure host output file ownership/permissions are sane

### Phase 5: Promote explicit image-oriented inputs

- add or prioritize:
  - `--dockerfile <path>`
  - `--context <dir>`
  - `--image <ref>`
- remove auto-detect as the default mental model

### Phase 6: Keep `push` unchanged

- continue uploading ext4 synchronously to `/bundles/push`
- optionally add `build --push` convenience later

---

## Non-Goals

- making users manually run `docker build`, `docker save`, or `/bundles/upload`
- teaching the Dimension CLI how every language ecosystem works
- keeping JS/Rust auto-detection as the primary build contract
- making Docker tar upload the main product workflow

---

## Summary

The correct long-term model is:

- **public UX**: `dimension build` → `dimension push`
- **artifact**: ext4
- **build abstraction**: explicit Dockerfile/image input
- **portability strategy**: use Docker internally on non-Linux hosts
- **language support**: anything that can run in a Linux image, not just Rust/JS

In short:

> Dimension should build ext4 bundles from explicit runtime images, using Docker as a portable Linux builder backend when necessary, and should stop relying on source-language auto-detection as the primary interface.
