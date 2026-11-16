//! `sd_notify` over the `NOTIFY_SOCKET` datagram socket: `READY=1`,
//! `STATUS=`, `STOPPING=1`. No library, one `UnixDatagram`.
//!
//! Under launchd there is no such socket: the runner records the same
//! moments as the phase of `run/state.json` (`runstate`), `ready` for
//! `READY=1` and `stopping` for `STOPPING=1`. The status line has no
//! counterpart there; the web URL is in the `web-url` file either way.

use std::ffi::OsStr;
use std::io;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::net::SocketAddr;
use std::os::unix::net::UnixDatagram;
use std::path::Path;

use crate::fsx;
use crate::name::RuntimeDir;
use crate::platform::launchd::runstate::{self, Phase};
use crate::runner::log_line;

/// The variable systemd sets for a `Type=notify` service.
const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";

/// Where the runner reports readiness and the start of a stop.
#[derive(Debug)]
pub struct Notify {
    channel: Channel,
}

/// The service manager's channel.
#[derive(Debug)]
enum Channel {
    /// systemd's connected notify socket.
    Socket(UnixDatagram),
    /// The run directory whose `state.json` the launchd runner keeps.
    StateFile(RuntimeDir),
}

impl Notify {
    /// Connects to `$NOTIFY_SOCKET`. `None` when unset (running outside
    /// systemd, as the checks do).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let address = std::env::var_os(NOTIFY_SOCKET)?;
        match connect(&address) {
            Ok(socket) => Some(Self {
                channel: Channel::Socket(socket),
            }),
            Err(error) => {
                log_line(&format!(
                    "cannot reach the service manager through {NOTIFY_SOCKET}: {error}"
                ));
                None
            }
        }
    }

    /// The launchd runner's channel: the phase of `<run dir>/state.json`,
    /// which the runner wrote (phase `starting`) before it spawned `dsh`.
    pub(crate) fn state_file(run_dir: RuntimeDir) -> Self {
        Self {
            channel: Channel::StateFile(run_dir),
        }
    }

    /// `READY=1`, or phase `ready`.
    pub fn ready(&self) {
        match &self.channel {
            Channel::Socket(socket) => send(socket, "READY=1"),
            Channel::StateFile(run_dir) => set_phase(run_dir, Phase::Ready),
        }
    }

    /// `STATUS=<text>`; nothing under launchd.
    pub fn status(&self, text: &str) {
        if let Channel::Socket(socket) = &self.channel {
            // One message is newline-separated assignments; keep the text on one line.
            send(socket, &format!("STATUS={}", text.replace('\n', " ")));
        }
    }

    /// `STOPPING=1`, or phase `stopping`.
    pub fn stopping(&self) {
        match &self.channel {
            Channel::Socket(socket) => send(socket, "STOPPING=1"),
            Channel::StateFile(run_dir) => set_phase(run_dir, Phase::Stopping),
        }
    }
}

/// Sends one message. A failure is logged and otherwise ignored: the
/// service manager's view is a report, never a reason to stop `dsh`.
fn send(socket: &UnixDatagram, message: &str) {
    if let Err(error) = socket.send(message.as_bytes()) {
        log_line(&format!(
            "cannot notify the service manager ({message}): {error}"
        ));
    }
}

/// Moves the run to `phase`. A failure is logged and otherwise ignored, as
/// a failed notification is.
fn set_phase(run_dir: &RuntimeDir, phase: Phase) {
    if let Err(error) = runstate::set_phase(run_dir, phase, fsx::unix_now()) {
        log_line(&format!("cannot record the run's phase {phase:?}: {error}"));
    }
}

/// An unbound datagram socket connected to `address`: a path, or an
/// abstract name written with a leading `@`.
fn connect(address: &OsStr) -> io::Result<UnixDatagram> {
    let socket = UnixDatagram::unbound()?;
    match address.as_bytes().strip_prefix(b"@") {
        Some(name) => connect_abstract(&socket, name)?,
        None => socket.connect(Path::new(address))?,
    }
    Ok(socket)
}

/// Connects to an abstract socket name, which only Linux has.
#[cfg(target_os = "linux")]
fn connect_abstract(socket: &UnixDatagram, name: &[u8]) -> io::Result<()> {
    socket.connect_addr(&SocketAddr::from_abstract_name(name)?)
}

/// Abstract socket names exist only on Linux; elsewhere such an address
/// is refused, never read as a path.
#[cfg(not(target_os = "linux"))]
fn connect_abstract(_socket: &UnixDatagram, _name: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "abstract socket names exist only on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(socket: &UnixDatagram) -> String {
        let mut buffer = [0_u8; 512];
        let length = socket.recv(&mut buffer).unwrap();
        String::from_utf8(buffer.get(..length).unwrap().to_vec()).unwrap()
    }

    // catches: notify datagrams omit READY or STOPPING, reorder them, or allow a newline to inject another status field
    #[test]
    fn sends_ready_to_temp_socket() {
        let dir = std::env::temp_dir().join(format!("dshd-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify.sock");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let manager = UnixDatagram::bind(&path).unwrap();
        let notify = Notify {
            channel: Channel::Socket(connect(path.as_os_str()).unwrap()),
        };
        notify.ready();
        notify.status("web http://127.0.0.1:1/?token=REDACTED\nsecond line");
        notify.stopping();
        assert_eq!(receive(&manager), "READY=1");
        assert_eq!(
            receive(&manager),
            "STATUS=web http://127.0.0.1:1/?token=REDACTED second line"
        );
        assert_eq!(receive(&manager), "STOPPING=1");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // catches: a systemd abstract notify address is treated as a filesystem path and readiness never reaches the manager
    #[cfg(target_os = "linux")]
    #[test]
    fn connects_to_abstract_socket() {
        let name = format!("dshd-notify-test-{}", std::process::id());
        let manager =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
                .unwrap();
        let notify = Notify {
            channel: Channel::Socket(connect(OsStr::new(&format!("@{name}"))).unwrap()),
        };
        notify.ready();
        assert_eq!(receive(&manager), "READY=1");
    }

    // catches: connecting to a nonexistent notify socket fabricates a successful connection
    #[test]
    fn unreachable_socket_is_an_error() {
        assert!(connect(OsStr::new("@dshd-notify-test-nobody-listens")).is_err());
    }

    // catches: the launchd runner's readiness or stop never reaching run/state.json, or a phase change that
    // rewrites the run's invocation or pid, so `start` waits out its whole deadline
    #[test]
    fn state_file_channel_moves_the_phase() {
        use crate::fsx::testing::Scratch;
        use crate::platform::launchd::runstate::RunState;

        let scratch = Scratch::new("notify-state");
        let run_dir = RuntimeDir::new(scratch.path().to_path_buf());
        let starting = RunState {
            invocation_id: "0123456789abcdef0123456789abcdef".to_owned(),
            pid: 4242,
            process_start: 1_700_000_000,
            phase: Phase::Starting,
            phase_since: 1_700_000_000,
        };
        runstate::write(&run_dir, &starting).unwrap();
        let notify = Notify::state_file(run_dir.clone());
        notify.ready();
        notify.status("web http://127.0.0.1:1/?token=REDACTED");
        let ready = runstate::read(&run_dir).unwrap().unwrap();
        assert_eq!(ready.phase, Phase::Ready);
        assert_eq!(
            (ready.invocation_id.as_str(), ready.pid, ready.process_start),
            ("0123456789abcdef0123456789abcdef", 4242, 1_700_000_000)
        );
        notify.stopping();
        assert_eq!(
            runstate::read(&run_dir).unwrap().unwrap().phase,
            Phase::Stopping
        );
    }
}
