//! Hyphae guest VM init binary (PID 1).
//!
//! This binary runs as the init process inside a Firecracker microVM.
//! It mounts essential filesystems, starts the dimension-agent (which
//! handles vsock communication and spawns the entrypoint), waits for
//! it to exit, then triggers a VM reboot (Firecracker exits cleanly).
//!
//! If dimension-agent is not present in the rootfs, the entrypoint
//! runs directly with no special payload delivery.
//!
//! Target: x86_64-unknown-linux-musl (statically linked, zero runtime deps)

/// Path to the dimension-agent binary. If present, it will be started
/// as a background process before the application entrypoint.
#[cfg(target_os = "linux")]
const DIMENSION_AGENT_PATH: &str = "/sbin/dimension-agent";

/// Well-known vsock port for host-guest communication.
/// Must match `dimension_protocol::VSOCK_PORT` (1024).
#[cfg(target_os = "linux")]
const DIMENSION_VSOCK_PORT: &str = "1024";

/// Platform proxy port used by the SDK (HTTP on localhost inside the VM).
#[cfg(target_os = "linux")]
const PLATFORM_PROXY_PORT: u32 = 8765;

/// vsock CID for the host (Firecracker convention).
#[cfg(target_os = "linux")]
const VMADDR_CID_HOST: u32 = 2;


/// Parse the null-separated entrypoint file into its binary and arguments.
///
/// Empty arguments are preserved. A single final empty part is ignored when
/// the file ends with a NUL terminator.
fn parse_entrypoint(entrypoint: &[u8]) -> Result<(String, Vec<String>), String> {
    let mut parts: Vec<&[u8]> = entrypoint.split(|&byte| byte == 0).collect();
    if entrypoint.ends_with(&[0]) {
        parts.pop();
    }

    let Some(binary) = parts.first() else {
        return Err("entrypoint file is empty".to_string());
    };
    if binary.is_empty() {
        return Err("entrypoint file is empty".to_string());
    }

    let binary = std::str::from_utf8(binary)
        .map_err(|e| format!("entrypoint binary is not valid UTF-8: {e}"))?
        .to_string();
    let args = parts[1..]
        .iter()
        .map(|arg| {
            std::str::from_utf8(arg)
                .map(str::to_string)
                .map_err(|e| format!("entrypoint argument is not valid UTF-8: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok((binary, args))
}

/// Serialize entrypoint arguments for transport in an environment variable.
fn serialize_args_json(args: &[String]) -> Result<String, serde_json::Error> {
    serde_json::to_string(args)
}

/// Testable core logic: returns `true` if the cmdline string contains the token
/// `hyphae.volume=true` as a whitespace-delimited token.
///
/// Only the exact token matches — `hyphae.volume=false` or `hyphae.volume=1`
/// do NOT count. This mirrors the pattern used by `read_runtime_env_from_cmdline`.
fn volume_flag_from_cmdline_str(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|t| t == "hyphae.volume=true")
}

/// Read the volume flag from `/proc/cmdline`.
///
/// Returns `true` if `hyphae.volume=true` is present as a whitespace-delimited
/// token on the kernel command line. Returns `false` if the file cannot be read
/// or the flag is absent.
#[cfg(target_os = "linux")]
fn read_volume_flag_from_cmdline() -> bool {
    let cmdline = match std::fs::read_to_string("/proc/cmdline") {
        Ok(c) => c,
        Err(_) => return false,
    };
    volume_flag_from_cmdline_str(&cmdline)
}

/// Set up OverlayFS with pivot_root so the rootfs becomes immutable and
/// all writes go to the persistent volume (/dev/vdb).
///
/// Steps:
///   1. Mount /dev/vdb at /mnt/persist (ext4)
///   2. Create overlay dirs (upper, work, workspace) on the volume
///   3. Mount overlayfs at /mnt/merged (lower=rootfs, upper+work on volume)
///   4. Move /mnt/persist mount into the new root tree
///   5. Bind-mount workspace directly (bypasses overlay)
///   6. pivot_root to /mnt/merged, detach old root
///   7. Remount /proc, /sys, /dev in the new root
///
/// Returns `true` on success, `false` on fatal error (caller must reboot).
/// The overlay upper+work dirs persist across reboots — file modifications
/// accumulate across VM turns.
#[cfg(target_os = "linux")]
fn setup_overlay_root() -> bool {
    // --- 1. Verify /dev/vdb ---
    if !std::path::Path::new("/dev/vdb").exists() {
        eprintln!("hyphae-init: fatal: volume required but /dev/vdb not present");
        return false;
    }

    // --- 2. Create mount points ---
    for dir in &["/mnt/persist", "/mnt/merged"] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("hyphae-init: fatal: mkdir {dir}: {e}");
            return false;
        }
    }

    // --- 3. Mount /dev/vdb ---
    if !raw_mount("/dev/vdb", "/mnt/persist", "ext4", 0, None) {
        return false;
    }
    eprintln!("hyphae-init: mounted /dev/vdb at /mnt/persist (ext4)");

    // --- 4. Create overlay dirs on persistent volume ---
    for dir in &[
        "/mnt/persist/upper",
        "/mnt/persist/work",
        "/mnt/persist/workspace",
    ] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("hyphae-init: fatal: mkdir {dir}: {e}");
            return false;
        }
    }

    // --- 5. Mount overlayfs ---
    if !raw_mount(
        "overlay",
        "/mnt/merged",
        "overlay",
        0,
        Some("lowerdir=/,upperdir=/mnt/persist/upper,workdir=/mnt/persist/work"),
    ) {
        return false;
    }
    eprintln!("hyphae-init: mounted overlayfs at /mnt/merged");

    // --- 6. Move the persist mount into the new root tree ---
    let _ = std::fs::create_dir_all("/mnt/merged/mnt/persist");
    if !raw_mount(
        "/mnt/persist",
        "/mnt/merged/mnt/persist",
        "",
        libc::MS_MOVE,
        None,
    ) {
        return false;
    }

    // --- 7. Bind-mount workspace (direct on volume, not through overlay) ---
    let _ = std::fs::create_dir_all("/mnt/merged/workspace");
    if !raw_mount(
        "/mnt/merged/mnt/persist/workspace",
        "/mnt/merged/workspace",
        "",
        libc::MS_BIND,
        None,
    ) {
        return false;
    }

    // --- 8. pivot_root to the overlay ---
    let _ = std::fs::create_dir_all("/mnt/merged/.oldroot");
    let new_root = c_str("/mnt/merged");
    let put_old = c_str("/mnt/merged/.oldroot");
    let ret = unsafe {
        libc::syscall(libc::SYS_pivot_root, new_root.as_ptr(), put_old.as_ptr())
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("hyphae-init: fatal: pivot_root: {err}");
        return false;
    }

    // Enter the new root
    let slash = c_str("/");
    unsafe {
        libc::chdir(slash.as_ptr());
    }

    // --- 9. Detach old root ---
    let oldroot = c_str("/.oldroot");
    unsafe {
        libc::umount2(oldroot.as_ptr(), libc::MNT_DETACH);
    }
    let _ = std::fs::remove_dir("/.oldroot");

    // --- 10. Remount essential filesystems in the new root ---
    mount_or_warn("devtmpfs", "/dev", "devtmpfs");
    mount_or_warn("proc", "/proc", "proc");
    mount_or_warn("sysfs", "/sys", "sysfs");

    eprintln!("hyphae-init: overlay root active — rootfs immutable, writes go to /dev/vdb");
    true
}

/// Low-level mount wrapper. Prints a fatal error and returns `false` on failure.
/// Pass an empty `fstype` for flag-only mounts (MS_MOVE, MS_BIND).
#[cfg(target_os = "linux")]
fn raw_mount(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> bool {
    let src_c = c_str(source);
    let tgt_c = c_str(target);
    let fst_c = if fstype.is_empty() {
        None
    } else {
        Some(c_str(fstype))
    };
    let data_c = data.map(c_str);

    let fst_ptr = fst_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let data_ptr = data_c
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr() as *const libc::c_void);

    let ret = unsafe {
        libc::mount(src_c.as_ptr(), tgt_c.as_ptr(), fst_ptr, flags, data_ptr)
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("hyphae-init: fatal: mount {source} at {target}: {err}");
        return false;
    }
    true
}

/// Create a [`CString`] from a `&str`. Panics if the string contains a null
/// byte (all callers pass known-good literals).
#[cfg(target_os = "linux")]
fn c_str(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).expect("c_str: null byte in string")
}

/// Read runtime env vars injected via kernel boot args from `/proc/cmdline`.
///
/// The launch paths (direct and jailed) encode runtime env vars as
/// `hyphae.env.KEY=VALUE` kernel command line parameters (see
/// `hyphae-core/src/launch/mod.rs`). This function reads `/proc/cmdline`
/// and returns the decoded key-value pairs via [`parse_runtime_env`].
///
/// Values may contain percent-encoded characters: `%20` → space, `%3D` → `=`,
/// `%0A` → newline, `%25` → `%`.
#[cfg(target_os = "linux")]
fn read_runtime_env_from_cmdline() -> Vec<(String, String)> {
    match std::fs::read_to_string("/proc/cmdline") {
        Ok(c) => parse_runtime_env(&c),
        Err(_) => Vec::new(),
    }
}

/// Parse `hyphae.env.KEY=VALUE` tokens out of a kernel command line.
///
/// Pure function shared with tests: the hyphae-core encoder
/// (`launch::env_boot_args`) and this decoder are kept in lockstep by the
/// round-trip tests at the bottom of this file.
fn parse_runtime_env(cmdline: &str) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    const PREFIX: &str = "hyphae.env.";

    for token in cmdline.split_whitespace() {
        if let Some(kv) = token.strip_prefix(PREFIX)
            && let Some((k, v)) = kv.split_once('=')
        {
            let key = percent_decode(k);
            let val = percent_decode(v);
            vars.push((key, val));
        }
    }
    vars
}

/// Decode percent-encoded sequences in a string.
/// Handles: %25 → %, %20 → space, %3D → =, %0A → newline.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte_val) = u8::from_str_radix(hex, 16) {
                result.push(byte_val as char);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}



/// Start the vsock↔TCP platform bridge in a background thread.
///
/// Listens on TCP 127.0.0.1:8765 and for each connection, opens a vsock
/// connection to the host (CID=2, port 8765). Bidirectional byte copy
/// bridges the SDK's HTTP requests through the Firecracker vsock channel
/// to the dimension-worker on the host, which forwards to the gateway.
///
/// This allows the SDK to talk plain HTTP to localhost:8765 with zero
/// changes — the bridge is transparent.
#[cfg(target_os = "linux")]
fn start_platform_bridge() {
    std::thread::spawn(|| {
        let listener = match std::net::TcpListener::bind("127.0.0.1:8765") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("hyphae-init: platform bridge: failed to bind TCP 127.0.0.1:8765: {e}");
                return;
            }
        };
        eprintln!("hyphae-init: platform bridge listening on 127.0.0.1:8765");

        for stream in listener.incoming() {
            match stream {
                Ok(tcp_stream) => {
                    std::thread::spawn(move || {
                        if let Err(e) = bridge_to_host(tcp_stream) {
                            eprintln!("hyphae-init: platform bridge error: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("hyphae-init: platform bridge accept error: {e}");
                }
            }
        }
    });
}

/// Bridge a single TCP connection to a vsock connection to the host.
///
/// Opens an AF_VSOCK socket, connects to CID=2 (host) port 8765, then
/// copies bytes bidirectionally between the TCP and vsock streams.
#[cfg(target_os = "linux")]
fn bridge_to_host(tcp_stream: std::net::TcpStream) -> std::io::Result<()> {
    use std::os::unix::io::{AsRawFd, FromRawFd};

    // Create AF_VSOCK socket.
    let vsock_fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if vsock_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Connect to host CID=2, port=8765.
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as u16;
    addr.svm_port = PLATFORM_PROXY_PORT;
    addr.svm_cid = VMADDR_CID_HOST;

    let ret = unsafe {
        libc::connect(
            vsock_fd,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(vsock_fd); }
        return Err(err);
    }

    // Wrap the vsock fd as a File for Read/Write (File drop closes the fd).
    let vsock_read: std::fs::File = unsafe { std::fs::File::from_raw_fd(vsock_fd) };
    let vsock_write = vsock_read.try_clone()?;

    let tcp_read = tcp_stream.try_clone()?;
    let tcp_write = tcp_stream;

    // Two threads for bidirectional copy.
    let t1 = std::thread::spawn(move || {
        let mut src = tcp_read;
        let mut dst = vsock_write;
        let _ = std::io::copy(&mut src, &mut dst);
        // Signal EOF to the vsock peer.
        unsafe { libc::shutdown(dst.as_raw_fd(), libc::SHUT_WR); }
    });

    let t2 = std::thread::spawn(move || {
        let mut src = vsock_read;
        let mut dst = tcp_write;
        let _ = std::io::copy(&mut src, &mut dst);
        let _ = dst.shutdown(std::net::Shutdown::Write);
    });

    let _ = t1.join();
    let _ = t2.join();
    Ok(())
}

#[cfg(target_os = "linux")]
fn main() {
    // Mount essential filesystems (non-fatal -- kernel may have auto-mounted some)
    mount_or_warn("devtmpfs", "/dev", "devtmpfs");
    mount_or_warn("proc", "/proc", "proc");
    mount_or_warn("sysfs", "/sys", "sysfs");

    // OverlayFS setup: opt-in via kernel cmdline flag.
    // If hyphae.volume=true is set, set up an overlay root so the rootfs
    // is immutable and ALL writes go to /dev/vdb. Includes pivot_root and
    // remounting /proc, /sys, /dev. Failure is FATAL.
    let volume_mounted = if read_volume_flag_from_cmdline() {
        if !setup_overlay_root() {
            reboot();
        }
        true
    } else {
        false
    };

    // Read entrypoint command from /etc/hyphae/entrypoint
    // Format: null-separated parts (first = binary path, rest = arguments)
    let entrypoint = match std::fs::read("/etc/hyphae/entrypoint") {
        Ok(data) => data,
        Err(e) => {
            eprintln!("hyphae-init: failed to read /etc/hyphae/entrypoint: {e}");
            reboot();
        }
    };

    let (cmd, args) = match parse_entrypoint(&entrypoint) {
        Ok(entrypoint) => entrypoint,
        Err(e) => {
            eprintln!("hyphae-init: {e}");
            reboot();
        }
    };

    // If dimension-agent is present, use it — it handles vsock communication
    // with the worker and spawns the entrypoint. Otherwise run the entrypoint
    // directly (no special payload delivery).
    if std::path::Path::new(DIMENSION_AGENT_PATH).exists() {
        // Start the platform vsock bridge: TCP localhost:8765 → vsock CID=2:8765
        // so the SDK inside the VM can reach the gateway via the host.
        start_platform_bridge();

        eprintln!("hyphae-init: starting dimension-agent");
        let args_json = match serialize_args_json(&args) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("hyphae-init: failed to serialize entrypoint arguments: {e}");
                reboot();
            }
        };
        let mut agent_cmd = std::process::Command::new(DIMENSION_AGENT_PATH);
        agent_cmd
            .env("DIMENSION_AGENT_PORT", DIMENSION_VSOCK_PORT)
            .env("DIMENSION_AGENT_BINARY", &cmd);

        // Pass build-time env vars from /etc/hyphae/env
        if let Ok(env_data) = std::fs::read_to_string("/etc/hyphae/env") {
            for line in env_data.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    agent_cmd.env(key, value);
                }
            }
        }

        // Runtime env vars from kernel cmdline override build-time vars
        for (key, value) in read_runtime_env_from_cmdline() {
            agent_cmd.env(key, value);
        }

        agent_cmd.env("DIMENSION_AGENT_ARGS_JSON", args_json);

        if volume_mounted {
            agent_cmd.env("DIMENSION_WORKSPACE", "/workspace");
        }

        match agent_cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
        {
            Ok(mut child) => {
                eprintln!("hyphae-init: dimension-agent started, waiting for exit");
                match child.wait() {
                    Ok(status) => eprintln!("hyphae-init: dimension-agent exited with {status}"),
                    Err(e) => eprintln!("hyphae-init: failed to wait on dimension-agent: {e}"),
                }
            }
            Err(e) => eprintln!("hyphae-init: failed to start dimension-agent: {e}"),
        }
    } else {
        // No agent — run the entrypoint directly.
        eprintln!("hyphae-init: starting {cmd} {}", args.join(" "));

        let mut child_cmd = std::process::Command::new(&cmd);
        child_cmd.args(&args)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        // Apply build-time env vars from /etc/hyphae/env
        if let Ok(env_data) = std::fs::read_to_string("/etc/hyphae/env") {
            for line in env_data.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    child_cmd.env(key, value);
                }
            }
        }

        // Runtime env vars from kernel cmdline override build-time vars
        for (key, value) in read_runtime_env_from_cmdline() {
            child_cmd.env(key, value);
        }

        if volume_mounted {
            child_cmd.env("DIMENSION_WORKSPACE", "/workspace");
        }

        if let Ok(workdir) = std::fs::read_to_string("/etc/hyphae/workdir") {
            let workdir = workdir.trim();
            if !workdir.is_empty() {
                child_cmd.current_dir(workdir);
            }
        }

        match child_cmd.spawn() {
            Ok(mut child) => match child.wait() {
                Ok(status) => eprintln!("hyphae-init: app exited with {status}"),
                Err(e) => eprintln!("hyphae-init: failed to wait on app: {e}"),
            },
            Err(e) => eprintln!("hyphae-init: failed to exec app: {e}"),
        }
    }

    // ALL code paths must end with reboot -- never let PID 1 exit.
    reboot();
}

/// Mount a filesystem, printing a warning on failure (non-fatal).
///
/// Creates the mount point directory if it does not exist.
#[cfg(target_os = "linux")]
fn mount_or_warn(source: &str, target: &str, fstype: &str) {
    let _ = std::fs::create_dir_all(target);

    let source_c = match std::ffi::CString::new(source) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("hyphae-init: invalid mount source: {source}");
            return;
        }
    };
    let target_c = match std::ffi::CString::new(target) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("hyphae-init: invalid mount target: {target}");
            return;
        }
    };
    let fstype_c = match std::ffi::CString::new(fstype) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("hyphae-init: invalid mount fstype: {fstype}");
            return;
        }
    };

    let ret = unsafe {
        libc::mount(
            source_c.as_ptr(),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if ret != 0 {
        let errno = std::io::Error::last_os_error();
        eprintln!("hyphae-init: warning: mount {target} ({fstype}) failed: {errno}");
    }
}

/// Sync filesystems and trigger a VM reboot.
///
/// In Firecracker, `reboot(LINUX_REBOOT_CMD_RESTART)` causes the VM process
/// to exit cleanly. This function never returns.
#[cfg(target_os = "linux")]
fn reboot() -> ! {
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_RESTART);
    }
    // reboot should not return, but if it does (e.g., permission denied),
    // loop forever rather than letting PID 1 exit (which causes kernel panic)
    eprintln!("hyphae-init: reboot syscall returned -- looping forever");
    loop {
        unsafe {
            libc::pause();
        }
    }
}

// Non-Linux stub for development (macOS, etc.)
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("hyphae-init: this binary is designed to run as PID 1 inside a Linux VM");
    eprintln!("hyphae-init: it will not function correctly on this platform");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    // volume_flag_from_cmdline_str is not yet defined — these tests are RED.

    #[test]
    fn volume_flag_true_in_cmdline() {
        assert!(super::volume_flag_from_cmdline_str(
            "console=ttyS0 hyphae.volume=true reboot=k"
        ));
    }

    #[test]
    fn volume_flag_absent_from_cmdline() {
        assert!(!super::volume_flag_from_cmdline_str(
            "console=ttyS0 reboot=k"
        ));
    }

    #[test]
    fn volume_flag_empty_cmdline() {
        assert!(!super::volume_flag_from_cmdline_str(""));
    }

    #[test]
    fn volume_flag_false_not_matched() {
        // Only the exact token "hyphae.volume=true" matches; "false" must not.
        assert!(!super::volume_flag_from_cmdline_str("hyphae.volume=false"));
    }

    #[test]
    fn entrypoint_args_survive_parsing_and_json_roundtrip() {
        let input = "node\0/app/runner with spaces.mjs\0\0こんにちは\0--flag=value";
        let (binary, args) = super::parse_entrypoint(input.as_bytes()).expect("parse entrypoint");

        assert_eq!(binary, "node");
        assert_eq!(
            args,
            vec!["/app/runner with spaces.mjs", "", "こんにちは", "--flag=value"]
        );

        let json = super::serialize_args_json(&args).expect("serialize args");
        let decoded: Vec<String> = serde_json::from_str(&json).expect("decode args JSON");
        assert_eq!(decoded, args);
    }

    #[test]
    fn entrypoint_parser_ignores_one_trailing_nul_terminator() {
        let (binary, args) = super::parse_entrypoint(b"/bin/echo\0hello\0")
            .expect("parse entrypoint");

        assert_eq!(binary, "/bin/echo");
        assert_eq!(args, vec!["hello"]);
    }

}

#[cfg(test)]
mod env_roundtrip_tests {
    //! Parity tests: the hyphae-core launch encoder (`env_boot_args`) and the
    //! decoder used at guest boot (`parse_runtime_env` + `percent_decode`)
    //! must round-trip every value the manifest [env] section can carry.
    use std::collections::HashMap;

    fn roundtrip(vars: &[(&str, &str)]) -> Vec<(String, String)> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let cmdline = hyphae_core::launch::env_boot_args(&map).join(" ");
        // Embed the tokens in a realistic full cmdline, the way the kernel
        // presents /proc/cmdline, to prove neighbors and defaults are inert.
        let full = format!("console=ttyS0 reboot=k panic=1 {cmdline} hyphae.volume=true");
        super::parse_runtime_env(&full)
    }

    #[test]
    fn plain_vars_roundtrip() {
        let decoded = roundtrip(&[
            ("MODEL_BASE_URL", "http://192.168.105.168:8000/v1"),
            ("MODEL_NAME", "muse-glimmer-30b"),
        ]);
        let sorted: Vec<_> = decoded.into_iter().map(|(k, _)| k).collect();
        // Keys come back sorted (encoder emits sorted keys).
        assert_eq!(sorted, vec!["MODEL_BASE_URL", "MODEL_NAME"]);
    }

    #[test]
    fn spaces_percent_equals_newlines_roundtrip() {
        let decoded = roundtrip(&[("PROMPT", "key=val 100% sure\nsecond line")]);
        assert_eq!(
            decoded,
            vec![(
                "PROMPT".to_string(),
                "key=val 100% sure\nsecond line".to_string()
            )]
        );
    }

    #[test]
    fn equals_sign_in_key_roundtrips() {
        let decoded = roundtrip(&[("A=B", "x")]);
        assert_eq!(decoded, vec![("A=B".to_string(), "x".to_string())]);
    }

    #[test]
    fn empty_value_roundtrips() {
        let decoded = roundtrip(&[("EMPTY", "")]);
        assert_eq!(decoded, vec![("EMPTY".to_string(), "".to_string())]);
    }

    #[test]
    fn non_env_tokens_are_ignored() {
        let decoded = super::parse_runtime_env(
            "console=ttyS0 hyphae.volume=true hyphae.env.ONLY=one not-env.x=1",
        );
        assert_eq!(decoded, vec![("ONLY".to_string(), "one".to_string())]);
    }
}
