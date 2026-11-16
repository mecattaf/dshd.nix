//! A relay socket whose path is longer than `sun_path` holds: on macOS
//! the socket lives under the home (`<instance>/run/acp.sock`) and
//! `sun_path` takes 104 bytes, so the client reaches such a path from
//! inside its directory. On Linux the runtime path always fits; this test
//! makes one that does not, to exercise the same client code. The client
//! finds the socket under `$XDG_RUNTIME_DIR` on Linux, and on macOS in the
//! instance its working directory is, as the CLI starts it.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{self, Command, Stdio};
use std::thread;

use dshd::relay::protocol::{Handshake, HandshakeReply, ProbeLine};

use crate::common::TestResult;

/// `sun_path` on Linux, the NUL included.
const SUN_PATH: usize = 108;

// catches: `dshd-run probe` (and attach, which shares the connect) failing on a socket path longer
// than sun_path instead of reaching it from its directory
#[test]
fn a_socket_path_longer_than_sun_path_is_reached_from_its_directory() -> TestResult {
    let root = std::env::temp_dir().join(format!("dshd-long-socket-{}", process::id()));
    let runtime_root = root.join("r".repeat(SUN_PATH));
    // `<root>/dshd/long` is the runtime directory on Linux, the instance on macOS.
    let instance = runtime_root.join("dshd").join("long");
    let dir = if cfg!(target_os = "macos") {
        instance.join(dshd::platform::launchd::RUN_DIR)
    } else {
        instance.clone()
    };
    fs::create_dir_all(&dir)?;
    let socket = dir.join("acp.sock");
    assert!(socket.as_os_str().len() > SUN_PATH, "{}", socket.display());
    // The test binds through a short link to the same directory.
    let short = root.join("s");
    symlink(&dir, &short)?;
    let listener = UnixListener::bind(short.join("acp.sock"))?;
    let reply = ProbeLine {
        attached: false,
        attached_since: None,
        initialized: true,
        held_sessions: Vec::new(),
        pending_permissions: 0,
    };
    let mut answer = serde_json::to_vec(&HandshakeReply::Probe(reply.clone()))?;
    answer.push(b'\n');
    let relay = thread::spawn(move || -> std::io::Result<String> {
        let (stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        (&stream).write_all(&answer)?;
        Ok(line)
    });
    let mut probe = Command::new(env!("CARGO_BIN_EXE_dshd-run"));
    probe
        .args(["probe", "long"])
        .env_clear()
        .env("XDG_RUNTIME_DIR", &runtime_root)
        .stdin(Stdio::null());
    if cfg!(target_os = "macos") {
        probe.current_dir(&instance);
    }
    let output = probe.output()?;
    let probed = output.status.code() == Some(0);
    if !probed {
        // The fake relay still waits for a client: wake it, so the test fails instead of hanging.
        drop(UnixStream::connect(short.join("acp.sock")));
    }
    let asked = relay.join().map_err(|_| "the relay thread panicked")?;
    fs::remove_dir_all(&root)?;
    assert!(probed, "{}", String::from_utf8_lossy(&output.stderr));
    let asked: Handshake = serde_json::from_str(&asked?)?;
    assert_eq!(asked, Handshake::Probe);
    let printed: ProbeLine = serde_json::from_slice(&output.stdout)?;
    assert_eq!(printed, reply);
    Ok(())
}
