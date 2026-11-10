//! `sd_notify` over the `NOTIFY_SOCKET` datagram socket: `READY=1`,
//! `STATUS=`, `STOPPING=1`. No library, one `UnixDatagram`.

use std::ffi::OsStr;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::Path;

use crate::runner::log_line;

/// The variable systemd sets for a `Type=notify` service.
const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";

/// A connected notify socket.
#[derive(Debug)]
pub struct Notify {
    socket: UnixDatagram,
}

impl Notify {
    /// Connects to `$NOTIFY_SOCKET`. `None` when unset (running outside
    /// systemd, as the checks do).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let address = std::env::var_os(NOTIFY_SOCKET)?;
        match connect(&address) {
            Ok(socket) => Some(Self { socket }),
            Err(error) => {
                log_line(&format!(
                    "cannot reach the service manager through {NOTIFY_SOCKET}: {error}"
                ));
                None
            }
        }
    }

    /// `READY=1`.
    pub fn ready(&self) {
        self.send("READY=1");
    }

    /// `STATUS=<text>`.
    pub fn status(&self, text: &str) {
        // One message is newline-separated assignments; keep the text on one line.
        self.send(&format!("STATUS={}", text.replace('\n', " ")));
    }

    /// `STOPPING=1`.
    pub fn stopping(&self) {
        self.send("STOPPING=1");
    }

    /// Sends one message. A failure is logged and otherwise ignored: the
    /// service manager's view is a report, never a reason to stop `dsh`.
    fn send(&self, message: &str) {
        if let Err(error) = self.socket.send(message.as_bytes()) {
            log_line(&format!(
                "cannot notify the service manager ({message}): {error}"
            ));
        }
    }
}

/// An unbound datagram socket connected to `address`: a path, or an
/// abstract name written with a leading `@`.
fn connect(address: &OsStr) -> io::Result<UnixDatagram> {
    let socket = UnixDatagram::unbound()?;
    match address.as_bytes().strip_prefix(b"@") {
        Some(name) => socket.connect_addr(&SocketAddr::from_abstract_name(name)?)?,
        None => socket.connect(Path::new(address))?,
    }
    Ok(socket)
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
            socket: connect(path.as_os_str()).unwrap(),
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
    #[test]
    fn connects_to_abstract_socket() {
        let name = format!("dshd-notify-test-{}", std::process::id());
        let manager =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
                .unwrap();
        let notify = Notify {
            socket: connect(OsStr::new(&format!("@{name}"))).unwrap(),
        };
        notify.ready();
        assert_eq!(receive(&manager), "READY=1");
    }

    // catches: connecting to a nonexistent notify socket fabricates a successful connection
    #[test]
    fn unreachable_socket_is_an_error() {
        assert!(connect(OsStr::new("@dshd-notify-test-nobody-listens")).is_err());
    }
}
