# E2E / Infrastructure Tasks

## Task 1: Log Capture
**Status**: In Progress

### Goal
Pipe Firecracker stdout/stderr to a log file instead of `/dev/null` so guest serial console output is capturable.

### Changes needed
- `crates/hyphae-core/src/process/spawn.rs` — Add optional log file path to `SpawnConfig`, use `Stdio::from(file)` instead of `Stdio::null()` when set
- `crates/hyphae-core/src/process/types.rs` — Add `log_file: Option<PathBuf>` to `SpawnConfig`
- `crates/hyphae-core/src/launch/direct.rs` — Thread log file path through to SpawnConfig
- `crates/hyphae-core/src/launch/jailed.rs` — Same for jailed path
- `crates/hyphae-core/src/launch/mod.rs` — Add `log_file` to `LaunchConfig`
- `crates/hyphae-core/src/orchestrator/run.rs` — Default log file to `{runtime_dir}/console.log`
- `crates/hyphae/src/cli.rs` — No CLI changes needed initially (auto-create log file)
- Update e2e test to use `hyphae run` with log capture

### Design
- Log file lives at `~/.local/share/hyphae/vms/{vm_id}/console.log`
- Firecracker stdout+stderr both redirect to this file
- `hyphae inspect {vm_id}` could show the log path
- Future: `hyphae logs {vm_id}` command to tail/stream

---

## Task 2: Binary-based Build (Arbitrary Payloads)
**Status**: In Progress

### Goal
Refactor `hyphae build` so the core path takes a pre-built binary, not a source project. Remove the requirement for JS/Rust project type detection.

### New CLI interface
```
hyphae build --binary ./my-static-binary                    # single binary
hyphae build --binary ./my-app-dir --entrypoint /app/server # directory with entrypoint
```

### Changes needed
- `crates/hyphae/src/cli.rs` — Add `--binary` and `--entrypoint` to BuildArgs
- `crates/hyphae-core/src/rootfs/binary.rs` — New module: copy binary/dir into staging, write entrypoint
- `crates/hyphae-core/src/rootfs/image.rs` — `build_rootfs()` gets a new code path for binary mode
- `crates/hyphae-core/src/rootfs/detect.rs` — Make project detection optional (only used when --binary not set)
- `crates/hyphae-core/src/orchestrator/build.rs` — Add binary_path/entrypoint to BuildRequest
- `crates/hyphae-core/src/orchestrator/types.rs` — Update BuildRequest type
- Keep JS/Rust builders as optional convenience (maybe `hyphae build --rust ./project`)

### Design
- Core pipeline: binary → staging dir → embed init → mkfs.ext4
- No cargo, no node, no project type detection in the default path
- User is responsible for producing a static binary
- Entrypoint defaults to `/app/{binary_name}` for single file, required for directory mode
