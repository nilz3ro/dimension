//! Linux-only implementation of the dimension guest agent.

use std::io::Write as IoWrite;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use dimension_protocol::proto::{envelope, Done, Envelope, OutboundMessage, Request};
use dimension_protocol::{ProtocolCodec, VSOCK_PORT};
use prost::Message;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::Encoder;

const EVENTS_SOCK_PATH: &str = "/run/dimension/events.sock";
const EVENTS_SOCK_ENV: &str = "DIMENSION_EVENTS_SOCK";
const MAX_BUNDLE_BODY: usize = 256 * 1024;
const DRAIN_GRACE_MS: u64 = 500;

pub fn run() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("dimension-agent: failed to build tokio runtime");
    runtime.block_on(async {
        if let Err(e) = run_async().await {
            eprintln!("dimension-agent: fatal: {e}");
            std::process::exit(1);
        }
    });
}

async fn run_async() -> Result<(), String> {
    let port: u32 = std::env::var("DIMENSION_AGENT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(VSOCK_PORT);

    let binary = std::env::var("DIMENSION_AGENT_BINARY")
        .map_err(|_| "DIMENSION_AGENT_BINARY not set".to_string())?;

    eprintln!("dimension-agent: listening on vsock port {port}");

    let (listen_fd, conn_fd) = vsock_accept(port)?;
    unsafe { libc::close(listen_fd) };
    eprintln!("dimension-agent: connection accepted");

    // Move the vsock fd into a std::fs::File. All vsock I/O happens on
    // blocking threads via spawn_blocking; tokio's async wrappers don't
    // safely support arbitrary AF_VSOCK fds without an AsyncFd adapter.
    let std_conn = unsafe { std::fs::File::from_raw_fd(conn_fd) };

    // Read the initial Request envelope synchronously on a blocking task.
    let mut request_reader =
        std_conn.try_clone().map_err(|e| format!("clone vsock fd: {e}"))?;
    let (request_id, payload) =
        tokio::task::spawn_blocking(move || read_request(&mut request_reader))
            .await
            .map_err(|e| format!("read_request join: {e}"))??;

    eprintln!(
        "dimension-agent: received {} bytes of payload, spawning {binary}",
        payload.len()
    );

    // ----- UDS for bundle outbound messages
    if let Some(parent) = Path::new(EVENTS_SOCK_PATH).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(EVENTS_SOCK_PATH);
    let uds = UnixListener::bind(EVENTS_SOCK_PATH)
        .map_err(|e| format!("bind {EVENTS_SOCK_PATH}: {e}"))?;
    let _ = std::fs::set_permissions(
        EVENTS_SOCK_PATH,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o666),
    );

    // ----- Fan-in channel for OutboundMessages
    let (tx, rx) = mpsc::channel::<OutboundMessage>(256);
    let sequence = Arc::new(AtomicU64::new(0));

    // ----- Vsock writer: blocking thread that drains the channel.
    let writer_handle = std::thread::Builder::new()
        .name("dimension-agent-vsock-writer".to_string())
        .spawn({
            let request_id = request_id.clone();
            move || vsock_writer(std_conn, request_id, rx)
        })
        .map_err(|e| format!("spawn vsock writer thread: {e}"))?;

    // ----- Spawn the child
    let mut child = Command::new(&binary)
        .env(EVENTS_SOCK_ENV, EVENTS_SOCK_PATH)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {binary}: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| format!("write child stdin: {e}"))?;
        drop(stdin);
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout missing".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr missing".to_string())?;

    let stdout_task = tokio::spawn(stdout_collector(stdout, tx.clone(), sequence.clone()));
    let stderr_task = tokio::spawn(stderr_streamer(stderr, tx.clone(), sequence.clone()));
    let uds_task = tokio::spawn(uds_accept_loop(uds, tx.clone(), sequence.clone()));

    let exit_status = child.wait().await.map_err(|e| format!("child wait: {e}"))?;
    eprintln!("dimension-agent: entrypoint exited with {exit_status}");

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    tokio::time::sleep(std::time::Duration::from_millis(DRAIN_GRACE_MS)).await;
    uds_task.abort();
    let _ = std::fs::remove_file(EVENTS_SOCK_PATH);

    // Close the channel; writer thread sees the close and sends Done.
    drop(tx);
    let _ = tokio::task::spawn_blocking(move || writer_handle.join()).await;

    eprintln!("dimension-agent: done");
    Ok(())
}

// ===========================================================================
// vsock setup
// ===========================================================================

fn vsock_accept(port: u32) -> Result<(libc::c_int, libc::c_int), String> {
    let sockfd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if sockfd < 0 {
        return Err(format!("socket: {}", std::io::Error::last_os_error()));
    }

    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as u16;
    addr.svm_port = port;
    addr.svm_cid = libc::VMADDR_CID_ANY;

    let ret = unsafe {
        libc::bind(
            sockfd,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(format!("bind: {}", std::io::Error::last_os_error()));
    }

    let ret = unsafe { libc::listen(sockfd, 1) };
    if ret != 0 {
        return Err(format!("listen: {}", std::io::Error::last_os_error()));
    }

    eprintln!("dimension-agent: waiting for connection...");
    let mut peer: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
    let conn_fd = unsafe {
        libc::accept(
            sockfd,
            &mut peer as *mut libc::sockaddr_vm as *mut libc::sockaddr,
            &mut peer_len,
        )
    };
    if conn_fd < 0 {
        return Err(format!("accept: {}", std::io::Error::last_os_error()));
    }

    Ok((sockfd, conn_fd))
}

fn read_request(conn: &mut std::fs::File) -> Result<(String, Vec<u8>), String> {
    use std::io::Read;

    let mut varint_buf = [0u8; 10];
    let mut varint_len = 0;

    for i in 0..10 {
        let mut byte = [0u8; 1];
        conn.read_exact(&mut byte)
            .map_err(|e| format!("reading varint byte {i}: {e}"))?;
        varint_buf[i] = byte[0];
        varint_len = i + 1;
        if byte[0] & 0x80 == 0 {
            break;
        }
    }

    let msg_len = prost::decode_length_delimiter(&varint_buf[..varint_len])
        .map_err(|e| format!("decoding varint: {e}"))?;

    if msg_len > 4 * 1024 * 1024 {
        return Err(format!("message too large: {msg_len} bytes"));
    }

    let mut msg_buf = vec![0u8; msg_len];
    conn.read_exact(&mut msg_buf)
        .map_err(|e| format!("reading body ({msg_len} bytes): {e}"))?;

    let env = Envelope::decode(&msg_buf[..])
        .map_err(|e| format!("decoding envelope: {e}"))?;

    match env.payload {
        Some(envelope::Payload::Request(Request { payload })) => {
            Ok((env.request_id, payload.to_vec()))
        }
        Some(other) => Err(format!("expected Request payload, got {other:?}")),
        None => Err("empty envelope".to_string()),
    }
}

/// Blocking vsock writer: encodes each OutboundMessage as an Envelope frame,
/// writes it synchronously, and sends a final Done envelope when the channel
/// closes.
fn vsock_writer(
    mut conn: std::fs::File,
    request_id: String,
    mut rx: mpsc::Receiver<OutboundMessage>,
) {
    let mut codec = ProtocolCodec::default();
    let mut buf = BytesMut::new();

    while let Some(msg) = rx.blocking_recv() {
        buf.clear();
        let env = Envelope {
            request_id: request_id.clone(),
            payload: Some(envelope::Payload::Outbound(msg)),
        };
        if let Err(e) = codec.encode(env, &mut buf) {
            eprintln!("dimension-agent: encode outbound: {e}");
            continue;
        }
        if let Err(e) = conn.write_all(&buf) {
            eprintln!("dimension-agent: vsock write: {e}");
            return;
        }
    }

    buf.clear();
    let done = Envelope {
        request_id,
        payload: Some(envelope::Payload::Done(Done::default())),
    };
    if let Err(e) = codec.encode(done, &mut buf) {
        eprintln!("dimension-agent: encode done: {e}");
        return;
    }
    if let Err(e) = conn.write_all(&buf) {
        eprintln!("dimension-agent: vsock write done: {e}");
    }
    let _ = conn.flush();
}

// ===========================================================================
// Source tasks
// ===========================================================================

async fn stdout_collector(
    mut stdout: tokio::process::ChildStdout,
    tx: mpsc::Sender<OutboundMessage>,
    seq: Arc<AtomicU64>,
) {
    let mut buf = Vec::new();
    let _ = stdout.read_to_end(&mut buf).await;
    if buf.is_empty() {
        return;
    }
    let msg = OutboundMessage {
        sequence: seq.fetch_add(1, Ordering::SeqCst),
        timestamp_ms: now_ms(),
        kind: "stdout-final".to_string(),
        content_type: "application/octet-stream".to_string(),
        body: Bytes::from(buf),
        attributes: Default::default(),
    };
    let _ = tx.send(msg).await;
}

async fn stderr_streamer(
    stderr: tokio::process::ChildStderr,
    tx: mpsc::Sender<OutboundMessage>,
    seq: Arc<AtomicU64>,
) {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let msg = OutboundMessage {
            sequence: seq.fetch_add(1, Ordering::SeqCst),
            timestamp_ms: now_ms(),
            kind: "stderr".to_string(),
            content_type: "text/plain; charset=utf-8".to_string(),
            body: Bytes::from(line.into_bytes()),
            attributes: Default::default(),
        };
        if tx.send(msg).await.is_err() {
            return;
        }
    }
}

async fn uds_accept_loop(
    uds: UnixListener,
    tx: mpsc::Sender<OutboundMessage>,
    seq: Arc<AtomicU64>,
) {
    loop {
        match uds.accept().await {
            Ok((stream, _addr)) => {
                let tx = tx.clone();
                let seq = seq.clone();
                tokio::spawn(uds_connection(stream, tx, seq));
            }
            Err(e) => {
                eprintln!("dimension-agent: uds accept error: {e}");
                return;
            }
        }
    }
}

async fn uds_connection(
    mut stream: tokio::net::UnixStream,
    tx: mpsc::Sender<OutboundMessage>,
    seq: Arc<AtomicU64>,
) {
    loop {
        // Frame: u32-LE length + JSON body.
        let mut len_buf = [0u8; 4];
        if stream.read_exact(&mut len_buf).await.is_err() {
            return;
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > MAX_BUNDLE_BODY {
            eprintln!("dimension-agent: uds frame {len} bytes out of range");
            return;
        }
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).await.is_err() {
            return;
        }
        match parse_uds_frame(&body) {
            Ok((kind, content_type, body_bytes, attrs)) => {
                let msg = OutboundMessage {
                    sequence: seq.fetch_add(1, Ordering::SeqCst),
                    timestamp_ms: now_ms(),
                    kind,
                    content_type,
                    body: Bytes::from(body_bytes),
                    attributes: attrs,
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                eprintln!("dimension-agent: malformed uds frame: {e}");
                return;
            }
        }
    }
}

fn parse_uds_frame(
    raw: &[u8],
) -> Result<(String, String, Vec<u8>, std::collections::HashMap<String, String>), String> {
    let v: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| format!("json: {e}"))?;
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .unwrap_or("bundle")
        .to_string();
    let content_type = v
        .get("contentType")
        .and_then(|x| x.as_str())
        .unwrap_or("application/json")
        .to_string();
    let body = v.get("body").ok_or_else(|| "missing body".to_string())?;
    let body_bytes = if body.is_string() {
        body.as_str().unwrap().as_bytes().to_vec()
    } else {
        serde_json::to_vec(body).map_err(|e| format!("body re-encode: {e}"))?
    };
    let attrs = v
        .get("attrs")
        .and_then(|x| x.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok((kind, content_type, body_bytes, attrs))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
