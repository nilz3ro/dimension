//! Dimension guest agent — vsock <-> child bridge.
//!
//! Runs inside a Firecracker microVM as a sidecar started by hyphae-init.
//!
//! Lifecycle:
//!   1. Listens on AF_VSOCK port 1024.
//!   2. Accepts one connection from the host worker.
//!   3. Reads a length-delimited protobuf `Request` envelope; extracts the
//!      JSON payload.
//!   4. Binds a Unix domain socket at `/run/dimension/events.sock` for
//!      outbound messages from the bundle (the path is exported as
//!      `DIMENSION_EVENTS_SOCK` to the child).
//!   5. Spawns the application entrypoint with the JSON payload on stdin.
//!   6. Multiplexes three sources onto the vsock connection as framed
//!      `OutboundMessage` envelopes:
//!        - UDS frames (length-prefixed JSON written by the bundle SDK)
//!        - The child's stdout (one final `stdout-final` message at exit)
//!        - The child's stderr (one `stderr` message per line)
//!   7. Drains pending events when the child exits, then sends `Done`.
//!
//! Target: x86_64-unknown-linux-musl (statically linked).

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dimension-agent: this binary only runs inside a Linux VM");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
mod agent;

#[cfg(target_os = "linux")]
fn main() {
    agent::run();
}
