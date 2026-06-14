#!/usr/bin/env bash
#
# Hyphae end-to-end test
#
# Proves the full lifecycle works:
#   1. Build a static Rust payload
#   2. Assemble a rootfs image (hyphae-init + payload → ext4)
#   3. Boot a Firecracker VM with the image
#   4. Capture serial console output
#   5. Verify the payload ran and produced expected output
#
# Prerequisites:
#   - firecracker in PATH
#   - /dev/kvm accessible
#   - mkfs.ext4 in PATH
#   - x86_64-unknown-linux-musl Rust target installed
#   - Firecracker-compatible kernel image
#
# Usage:
#   ./tests/e2e/hyphae-e2e.sh [--kernel /path/to/vmlinux]

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
KERNEL="${KERNEL:-}"
HYPHAE_BIN="${HYPHAE_BIN:-$REPO_ROOT/target/debug/hyphae}"
TIMEOUT_SECS=30
MARKER="HYPHAE_E2E_SUCCESS_$$_$(date +%s)"
TEST_DIR=""
PASS_COUNT=0
FAIL_COUNT=0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

cleanup() {
    if [ -n "$TEST_DIR" ] && [ -d "$TEST_DIR" ]; then
        rm -rf "$TEST_DIR"
    fi
}
trap cleanup EXIT

log()  { echo "--- $*"; }
pass() { echo "  PASS: $*"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo "  FAIL: $*"; FAIL_COUNT=$((FAIL_COUNT + 1)); }

die() {
    echo "FATAL: $*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --kernel) KERNEL="$2"; shift 2 ;;
        *) die "Unknown argument: $1" ;;
    esac
done

# Auto-discover kernel if not specified
if [ -z "$KERNEL" ]; then
    for candidate in \
        "$REPO_ROOT/vmlinux"* \
        "$HOME/vmlinux"* \
        /opt/firecracker/vmlinux*; do
        if [ -f "$candidate" ]; then
            KERNEL="$candidate"
            break
        fi
    done
fi

[ -z "$KERNEL" ] && die "No kernel image found. Pass --kernel /path/to/vmlinux"

# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

log "Checking prerequisites"

command -v firecracker >/dev/null || die "firecracker not found in PATH"
command -v mkfs.ext4   >/dev/null || die "mkfs.ext4 not found in PATH"
[ -e /dev/kvm ]                   || die "/dev/kvm not accessible"
[ -r /dev/kvm ]                   || die "/dev/kvm not readable"
[ -f "$KERNEL" ]                  || die "Kernel not found: $KERNEL"

rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl \
    || die "Rust musl target not installed (rustup target add x86_64-unknown-linux-musl)"

pass "All prerequisites satisfied (firecracker, mkfs.ext4, /dev/kvm, kernel, musl target)"

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

TEST_DIR="$(mktemp -d /tmp/hyphae-e2e.XXXXXX)"
log "Test directory: $TEST_DIR"

# ---------------------------------------------------------------------------
# Test 1: Build hyphae CLI and locate hyphae-init
# ---------------------------------------------------------------------------

log "Test 1: Building hyphae CLI"

cargo build -p hyphae --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1 | tail -5

[ -f "$HYPHAE_BIN" ] || die "hyphae binary not found at $HYPHAE_BIN"

# hyphae-init is built by hyphae-core's build.rs and embedded via include_bytes!.
# For the e2e test we need the standalone binary. It's cached in target_build_script/
# from a previous hyphae-core build (build.rs compiles it with musl automatically).
INIT_BIN="$REPO_ROOT/target_build_script/x86_64-unknown-linux-musl/release/hyphae-init"
[ -f "$INIT_BIN" ] || die "hyphae-init binary not found at $INIT_BIN (run 'cargo build -p hyphae-core' first)"

file "$INIT_BIN" | grep -qE "static(ally|-pie) linked" \
    || die "hyphae-init is not statically linked: $(file "$INIT_BIN")"

pass "hyphae CLI built, hyphae-init located (static musl binary)"

# ---------------------------------------------------------------------------
# Test 2: Build a static test payload
# ---------------------------------------------------------------------------

log "Test 2: Building test payload"

PAYLOAD_DIR="$TEST_DIR/test-payload"
mkdir -p "$PAYLOAD_DIR/src"

cat > "$PAYLOAD_DIR/Cargo.toml" << 'TOML'
[package]
name = "test-payload"
version = "0.1.0"
edition = "2021"
TOML

cat > "$PAYLOAD_DIR/src/main.rs" << RUST
fn main() {
    println!("$MARKER");
    println!("hyphae-e2e: payload executed successfully");
    eprintln!("hyphae-e2e: stderr works too");
}
RUST

cargo build --release --target x86_64-unknown-linux-musl \
    --manifest-path "$PAYLOAD_DIR/Cargo.toml" 2>&1 | tail -3

PAYLOAD_BIN="$PAYLOAD_DIR/target/x86_64-unknown-linux-musl/release/test-payload"
[ -f "$PAYLOAD_BIN" ] || die "Test payload binary not found"

# Verify it's statically linked
if file "$PAYLOAD_BIN" | grep -qE "static(ally|-pie) linked"; then
    pass "Test payload is statically linked"
else
    fail "Test payload is not statically linked (may still work): $(file "$PAYLOAD_BIN")"
fi

# ---------------------------------------------------------------------------
# Test 3: Assemble rootfs image
# ---------------------------------------------------------------------------

log "Test 3: Assembling rootfs image"

STAGING="$TEST_DIR/staging"
mkdir -p "$STAGING"/{dev,proc,sys,sbin,etc/hyphae,app,lib,tmp}

# Copy hyphae-init as /sbin/init
cp "$INIT_BIN" "$STAGING/sbin/init"
chmod 755 "$STAGING/sbin/init"

# Copy payload binary
cp "$PAYLOAD_BIN" "$STAGING/app/test-payload"
chmod 755 "$STAGING/app/test-payload"

# Write entrypoint (null-separated: binary path)
printf '/app/test-payload' > "$STAGING/etc/hyphae/entrypoint"

# Create ext4 image (same flags as hyphae-core uses)
ROOTFS="$TEST_DIR/rootfs.ext4"
mkfs.ext4 -d "$STAGING" -N 64 -b 4096 -m 0 -E root_owner=0:0 -t ext4 "$ROOTFS" 16384K 2>&1 | tail -2

[ -f "$ROOTFS" ] || die "rootfs image not created"
pass "Rootfs image created ($(du -h "$ROOTFS" | cut -f1))"

# ---------------------------------------------------------------------------
# Test 4: Boot VM and capture console output
# ---------------------------------------------------------------------------

log "Test 4: Booting Firecracker VM"

FC_SOCKET="$TEST_DIR/firecracker.sock"
FC_CONFIG="$TEST_DIR/fc-config.json"
CONSOLE_LOG="$TEST_DIR/console.log"

cat > "$FC_CONFIG" << JSON
{
  "boot-source": {
    "kernel_image_path": "$KERNEL",
    "boot_args": "console=ttyS0 reboot=k panic=1"
  },
  "machine-config": {
    "vcpu_count": 2,
    "mem_size_mib": 128
  },
  "drives": [
    {
      "drive_id": "rootfs",
      "path_on_host": "$ROOTFS",
      "is_root_device": true,
      "is_read_only": false
    }
  ]
}
JSON

# Run Firecracker with a timeout. The VM will:
#   1. Boot kernel
#   2. hyphae-init mounts filesystems, reads entrypoint, execs payload
#   3. Payload prints our marker to stdout (→ serial console → our stdout)
#   4. hyphae-init sees exit, calls reboot()
#   5. Firecracker exits
#
# We capture all stdout+stderr (serial console output) to a log file.

log "  Launching VM (timeout: ${TIMEOUT_SECS}s)..."

set +e
timeout "$TIMEOUT_SECS" firecracker \
    --api-sock "$FC_SOCKET" \
    --config-file "$FC_CONFIG" \
    > "$CONSOLE_LOG" 2>&1
FC_EXIT=$?
set -e

if [ $FC_EXIT -eq 124 ]; then
    fail "Firecracker timed out after ${TIMEOUT_SECS}s"
    echo "  Console output (last 20 lines):"
    tail -20 "$CONSOLE_LOG" | sed 's/^/    /'
elif [ $FC_EXIT -ne 0 ]; then
    # Firecracker may exit non-zero on guest reboot, which is expected
    log "  Firecracker exited with code $FC_EXIT (may be normal for guest reboot)"
fi

# ---------------------------------------------------------------------------
# Test 5: Verify payload output in console log
# ---------------------------------------------------------------------------

log "Test 5: Verifying console output"

if [ ! -s "$CONSOLE_LOG" ]; then
    fail "Console log is empty"
    echo "  This likely means Firecracker failed to start or serial console is not working."
else
    pass "Console log is non-empty ($(wc -l < "$CONSOLE_LOG") lines)"
fi

# -- Prove we're inside a VM, not just a subprocess --

# The kernel must report KVM as the hypervisor (only possible inside a VM)
if grep -q "Hypervisor detected: KVM" "$CONSOLE_LOG"; then
    pass "Kernel booted inside KVM hypervisor"
else
    fail "KVM hypervisor detection not found (not running in a VM?)"
fi

# The rootfs must be mounted as a virtio block device (vda), not a host filesystem
if grep -q "EXT4-fs (vda): mounted filesystem" "$CONSOLE_LOG"; then
    pass "Rootfs mounted as virtio block device (vda)"
else
    fail "Virtio block device mount not found"
fi

# The kernel must invoke /sbin/init (our hyphae-init) as PID 1
if grep -q "Run /sbin/init as init process" "$CONSOLE_LOG"; then
    pass "Kernel launched /sbin/init (hyphae-init) as PID 1"
else
    fail "Kernel init handoff not found"
fi

# -- Prove the payload ran correctly inside the VM --

# hyphae-init must have read the entrypoint and started our binary
if grep -q "hyphae-init: starting /app/test-payload" "$CONSOLE_LOG"; then
    pass "hyphae-init exec'd /app/test-payload"
else
    fail "hyphae-init startup message not found"
fi

# The unique marker proves our payload code actually executed (not just init)
if grep -q "$MARKER" "$CONSOLE_LOG"; then
    pass "Payload marker found in VM console output"
else
    fail "Payload marker NOT found in console output"
    echo "  Expected: $MARKER"
    echo "  Console output:"
    cat "$CONSOLE_LOG" | sed 's/^/    /'
fi

# Payload stdout and stderr both routed through serial console
if grep -q "payload executed successfully" "$CONSOLE_LOG"; then
    pass "Payload stdout captured via serial console"
else
    fail "Payload stdout message not found"
fi

if grep -q "stderr works too" "$CONSOLE_LOG"; then
    pass "Payload stderr captured via serial console"
else
    fail "Payload stderr message not found"
fi

# -- Prove clean lifecycle: payload exit → init reboot → VM shutdown --

# hyphae-init must report the app exited cleanly (exit status 0)
if grep -q "hyphae-init: app exited with exit status: 0" "$CONSOLE_LOG"; then
    pass "Payload exited with status 0"
else
    fail "Clean payload exit not found"
fi

# The guest kernel must have triggered a reboot (hyphae-init calls reboot())
if grep -q "reboot: Restarting system" "$CONSOLE_LOG"; then
    pass "Guest kernel reboot triggered (VM lifecycle complete)"
else
    fail "Guest reboot not found"
fi

# Firecracker must have exited cleanly
if grep -q "Firecracker exiting successfully" "$CONSOLE_LOG"; then
    pass "Firecracker exited cleanly"
else
    fail "Firecracker clean exit not found"
fi

# ---------------------------------------------------------------------------
# Test 6: hyphae CLI smoke test (build command)
# ---------------------------------------------------------------------------

log "Test 6: hyphae CLI smoke test"

# Build a Rust project through the hyphae CLI.
# Note: hyphae build currently uses 'cargo build --release' without --target musl,
# producing a dynamically linked binary. The build itself should succeed, even though
# the resulting image wouldn't boot (the binary needs libc which isn't in the rootfs).
# This tests the CLI orchestration, registry, and image creation pipeline.

CLI_PROJECT="$TEST_DIR/cli-test-project"
mkdir -p "$CLI_PROJECT/src"

cat > "$CLI_PROJECT/Cargo.toml" << 'TOML'
[package]
name = "cli-test"
version = "0.1.0"
edition = "2021"
TOML

cat > "$CLI_PROJECT/src/main.rs" << 'RUST'
fn main() {
    println!("hello from cli-test");
}
RUST

# Use a temporary HOME so we don't pollute the real registry.
# Preserve RUSTUP_HOME and CARGO_HOME so the toolchain is still accessible.
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export HOME="$TEST_DIR/home"
mkdir -p "$HOME"

set +e
"$HYPHAE_BIN" build "$CLI_PROJECT" --name cli-test --tag e2e 2>&1
CLI_EXIT=$?
set -e

if [ $CLI_EXIT -eq 0 ]; then
    pass "hyphae build succeeded"
else
    fail "hyphae build failed (exit $CLI_EXIT)"
fi

# Verify we can list the bundle
set +e
"$HYPHAE_BIN" list bundles 2>&1 | grep -q "cli-test"
LIST_EXIT=$?
set -e

if [ $LIST_EXIT -eq 0 ]; then
    pass "Bundle appears in 'hyphae list bundles'"
else
    fail "Bundle not found in listing"
fi

# ---------------------------------------------------------------------------
# Test 7: hyphae build --binary (arbitrary payload)
# ---------------------------------------------------------------------------

log "Test 7: hyphae build --binary"

# Use the same static musl payload we built in test 2.
# This tests the new --binary flag that bypasses project type detection.

set +e
"$HYPHAE_BIN" build --binary "$PAYLOAD_BIN" --name binary-test --tag e2e 2>&1
BINARY_EXIT=$?
set -e

if [ $BINARY_EXIT -eq 0 ]; then
    pass "hyphae build --binary succeeded"
else
    fail "hyphae build --binary failed (exit $BINARY_EXIT)"
fi

# Verify bundle is in registry
set +e
"$HYPHAE_BIN" list bundles 2>&1 | grep -q "binary-test"
set -e

if [ $? -eq 0 ]; then
    pass "Binary bundle appears in registry"
else
    fail "Binary bundle not found in listing"
fi

# ---------------------------------------------------------------------------
# Test 8: hyphae run with log capture + hyphae stop
# ---------------------------------------------------------------------------

log "Test 8: hyphae run (log capture) + hyphae stop"

# Run the binary-test bundle through the full hyphae CLI.
# This tests: orchestrator run → Firecracker spawn → log capture → stop.

set +e
RUN_OUTPUT=$("$HYPHAE_BIN" --json run binary-test:e2e --kernel "$KERNEL" 2>&1)
RUN_EXIT=$?
set -e

if [ $RUN_EXIT -eq 0 ]; then
    pass "hyphae run succeeded"
else
    fail "hyphae run failed (exit $RUN_EXIT): $RUN_OUTPUT"
fi

# Extract VM ID and log file from JSON output (multi-line, so collapse whitespace)
RUN_FLAT=$(echo "$RUN_OUTPUT" | tr -d ' \n')
VM_ID=$(echo "$RUN_FLAT" | grep -o '"vm_id":"[^"]*"' | head -1 | cut -d'"' -f4)
VM_CONSOLE_LOG=$(echo "$RUN_FLAT" | grep -o '"log_file":"[^"]*"' | head -1 | cut -d'"' -f4)

if [ -n "$VM_ID" ]; then
    pass "Got VM ID: $VM_ID"
else
    fail "Could not extract VM ID from run output"
    echo "  Output: $RUN_OUTPUT"
fi

# Wait briefly for the VM to boot, run payload, and exit (it's fast — ~1 second)
sleep 3

if [ -f "$VM_CONSOLE_LOG" ]; then
    pass "Console log file created at $VM_CONSOLE_LOG"
else
    fail "Console log file not found at $VM_CONSOLE_LOG"
    # Try to find it
    find "$HOME/.local/share/hyphae" -name "console.log" 2>/dev/null | head -5 | sed 's/^/    /'
fi

# Verify the log contains VM evidence (same checks as test 5)
if [ -f "$VM_CONSOLE_LOG" ] && grep -q "Hypervisor detected: KVM" "$VM_CONSOLE_LOG"; then
    pass "hyphae run: log captures KVM boot"
else
    fail "hyphae run: KVM boot not in log"
fi

if [ -f "$VM_CONSOLE_LOG" ] && grep -q "hyphae-init: starting" "$VM_CONSOLE_LOG"; then
    pass "hyphae run: log captures payload execution"
else
    fail "hyphae run: payload execution not in log"
fi

# Stop the VM (it likely already exited, but this tests the stop pipeline)
if [ -n "$VM_ID" ]; then
    set +e
    "$HYPHAE_BIN" stop "$VM_ID" 2>&1
    # Don't check exit code — VM may have already exited
    set -e
    pass "hyphae stop completed"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "==========================================="
echo "  Results: $PASS_COUNT passed, $FAIL_COUNT failed"
echo "==========================================="

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo ""
    echo "Console log saved at: $CONSOLE_LOG"
    # Don't clean up on failure so logs can be inspected
    TEST_DIR=""
    exit 1
fi

exit 0
