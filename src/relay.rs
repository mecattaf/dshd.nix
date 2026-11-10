//! The ACP relay inside the runner, and its only client. The runner owns
//! `dsh acp`'s stdio; a controller attaches over the instance's Unix
//! socket, one at a time. The relay answers `session/list` and the resume
//! of a held session from its own memory, buffers bounded output per held
//! session while no controller is attached, and never starts anything.
//!
//! Threads: one reads dsh's stdout and routes each frame, one accepts
//! connections, and each connection gets one that performs the handshake
//! and then, for the controller, routes its frames to dsh. A controller
//! that stops reading never blocks dsh, the probe or the next attach for
//! longer than `CONTROLLER_STALL`: then it is detached and the frames of
//! its held sessions go back to their rings.

pub mod attach;
pub mod held;
pub mod protocol;
mod pump;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;
use crate::name::{InstanceName, RuntimeDir};
use crate::relay::held::Held;
use crate::relay::protocol::{Handshake, HandshakeReply, ProbeLine, Refusal};
use crate::relay::pump::Frame;
use crate::runner::log_line;
use crate::{fsx, out, sys};

/// Frames buffered per held session while no controller is attached.
pub const RING_PER_SESSION: usize = 1024;
/// Bytes buffered across all held sessions; the oldest frames are dropped
/// and counted.
pub const RING_TOTAL_BYTES: usize = 8 * 1024 * 1024;
/// Largest single JSON-RPC frame accepted on either side.
pub const FRAME_CAP: usize = 64 * 1024 * 1024;
/// How long `probe` waits for the relay's answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a write to the controller may make no progress before the
/// relay detaches it; also the longest dsh's stdout waits for a controller
/// that stopped reading.
pub const CONTROLLER_STALL: Duration = Duration::from_secs(5);

/// Mode of the socket; the runtime directory around it is 0700.
const SOCKET_MODE: u32 = 0o600;
/// How long `child_exited` lets the frames dsh wrote before it exited
/// reach the controller before closing it.
const CHILD_EOF_GRACE: Duration = Duration::from_secs(1);
/// The variable naming the user's runtime root, where clients find the socket.
const RUNTIME_ROOT: &str = "XDG_RUNTIME_DIR";
/// The thread that routes dsh's stdout.
const CHILD_THREAD: &str = "dshd-acp-child";
/// The thread that accepts connections.
const ACCEPT_THREAD: &str = "dshd-acp-accept";
/// The thread of one connection.
const CONNECTION_THREAD: &str = "dshd-acp-conn";

/// The running relay, owned by the runner.
#[derive(Debug)]
pub struct Relay {
    shared: Arc<Shared>,
    socket: PathBuf,
    accept: JoinHandle<()>,
    pump: JoinHandle<()>,
}

/// What the relay's threads share. Locks are taken in the order `out`,
/// `state`; `child_in` is taken alone. `out` and `child_in` are held while
/// writing to their stream, `state` never is.
#[derive(Debug)]
struct Shared {
    /// The writing half of the attached controller.
    out: Mutex<Option<Controller>>,
    /// Everything the relay remembers.
    state: Mutex<State>,
    /// Signalled once dsh's stdout has ended and every frame was routed.
    closed: Condvar,
    /// dsh's stdin, until the relay closes it.
    child_in: Mutex<Option<ChildStdin>>,
    /// Set by `shutdown`; the accept loop ends at its next connection.
    stopping: AtomicBool,
}

/// The relay's memory.
#[derive(Debug, Default)]
struct State {
    held: Held,
    attached: Option<Attachment>,
    /// The generation of the last controller that attached.
    generation: u64,
    /// dsh answered an `initialize`.
    initialized: bool,
    /// dsh is gone or going: attaches are refused.
    exited: bool,
    /// dsh's stdout ended and every frame from it was routed.
    closed: bool,
    /// The last request id the relay gave dsh.
    next_id: u64,
    /// Controller requests dsh has not answered, by the relay's id.
    in_flight: BTreeMap<u64, InFlight>,
    /// Requests from dsh no controller has answered yet.
    waiting: Vec<ChildRequest>,
}

/// The attached controller as the probe reports it.
#[derive(Debug)]
struct Attachment {
    generation: u64,
    /// Unix seconds.
    since: u64,
    /// A handle that shuts the connection down, for a takeover.
    closer: UnixStream,
}

/// The writing half of the attached controller's connection.
#[derive(Debug)]
struct Controller {
    generation: u64,
    stream: UnixStream,
}

/// A controller request forwarded to dsh under the relay's own id.
#[derive(Debug)]
struct InFlight {
    /// The controller that sent it.
    generation: u64,
    /// Its id as the controller sent it.
    id: Value,
    tracked: Tracked,
}

/// What the relay learns from the answer to a forwarded request.
#[derive(Debug)]
enum Tracked {
    /// `initialize`: dsh is initialized once it answers.
    Initialize,
    /// `session/new`, or `session/resume` of a session the relay does not
    /// hold: the session is held once dsh answers.
    Open {
        /// The session resumed; a new one's id comes in the answer.
        session: Option<String>,
        cwd: String,
        mcp_servers: Value,
    },
    /// `session/close`: the session is forgotten once dsh answers.
    Close(String),
    /// `session/list` without a cursor: held sessions are prepended.
    List {
        /// The request's cwd filter.
        cwd: Option<String>,
    },
    /// Anything else.
    Other,
}

/// A request from dsh, kept until a controller answers it.
#[derive(Debug)]
struct ChildRequest {
    /// dsh's id, which the answer carries back.
    id: Value,
    session: Option<String>,
    /// A `session/request_permission`.
    permission: bool,
    frame: Vec<u8>,
}

impl Relay {
    /// Binds `socket` (0600 inside the 0700 runtime directory), takes the
    /// child's pipes and starts the pump threads.
    pub fn start(
        socket: &Path,
        child_stdin: ChildStdin,
        child_stdout: ChildStdout,
    ) -> Result<Self, Error> {
        let listener =
            UnixListener::bind(socket).map_err(|source| Error::io("bind", socket, source))?;
        // bind(2) takes no mode, and the 0700 runtime directory admits no one else meanwhile.
        let restricted = fs::set_permissions(socket, fs::Permissions::from_mode(SOCKET_MODE)); // gate: allow R8
        restricted.map_err(|source| Error::io("restrict", socket, source))?;
        let shared = Arc::new(Shared {
            out: Mutex::new(None),
            state: Mutex::new(State::default()),
            closed: Condvar::new(),
            child_in: Mutex::new(Some(child_stdin)),
            stopping: AtomicBool::new(false),
        });
        let pump = spawn(CHILD_THREAD, {
            let shared = Arc::clone(&shared);
            move || pump::child_pump(&shared, child_stdout)
        })?;
        let accept = spawn(ACCEPT_THREAD, {
            let shared = Arc::clone(&shared);
            move || accept_loop(&shared, &listener)
        })?;
        Ok(Self {
            shared,
            socket: socket.to_owned(),
            accept,
            pump,
        })
    }

    /// Stops dsh the ACP way, on the runner's main thread when a stop signal
    /// arrives: attaches are refused with `child-exited` from now on, every
    /// pending permission request is answered `cancelled`, and dsh's stdin
    /// is closed. The controller stays attached and receives what dsh still
    /// writes, its last answers included, until dsh exits. Returns false
    /// when stdin is busy or cannot accept a cancellation without blocking;
    /// the main supervisor must then signal its unreaped child.
    pub fn stop_child(&self) -> bool {
        lock(&self.shared.state).exited = true;
        let mut input = match self.shared.child_in.try_lock() {
            Ok(input) => input,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        let Some(stdin) = input.as_mut() else {
            return true;
        };
        if let Err(error) = sys::nonblocking_child_input(stdin) {
            log_line(&format!(
                "cannot prepare dsh's stdin for cancellation: {error}"
            ));
            return false;
        }
        let cancelled = pump::cancel_permissions(&self.shared, stdin);
        drop(input.take());
        cancelled
    }

    /// Tells the relay the child is gone, once the frames dsh wrote before
    /// it exited reached the controller: requests still in flight are
    /// answered -32603 "agent exited", the controller gets EOF and `attach`
    /// answers `refused` from now on.
    pub fn child_exited(&self) {
        let state = lock(&self.shared.state);
        let waited = self
            .shared
            .closed
            .wait_timeout_while(state, CHILD_EOF_GRACE, |state| !state.closed);
        let (state, wait) = waited.unwrap_or_else(PoisonError::into_inner);
        drop(state);
        if wait.timed_out() {
            log_line("dsh exited but its stdout is still open; closing the controller");
        }
        pump::wind_down(&self.shared);
    }

    /// Stops the threads and unlinks the socket, after `child_exited`:
    /// wakes the accept loop, closes any controller and joins the child
    /// pump once dsh's stdout has ended.
    pub fn shutdown(self) {
        let Self {
            shared,
            socket,
            accept,
            pump,
        } = self;
        shared.stopping.store(true, Ordering::SeqCst);
        match UnixStream::connect(&socket) {
            Ok(wake) => {
                drop(wake);
                join(accept, ACCEPT_THREAD);
            }
            Err(error) => log_line(&format!("cannot wake the relay's accept loop: {error}")),
        }
        pump::wind_down(&shared);
        if let Err(error) = fs::remove_file(&socket)
            && error.kind() != io::ErrorKind::NotFound
        {
            log_line(&Error::io("remove", &socket, error).to_string());
        }
        let closed = lock(&shared.state).closed;
        if closed {
            join(pump, CHILD_THREAD);
        }
    }
}

/// `dshd-run attach NAME [--takeover]`: connects to the socket under
/// `$XDG_RUNTIME_DIR/dshd/NAME`, performs the handshake, then pumps the
/// caller's stdio to the relay until either side ends. Exit 0 on a clean
/// end, 4 when refused (attached-elsewhere), 5 when the instance is not
/// running, 1 on I/O failure.
pub fn attach(name: &str, takeover: bool) -> u8 {
    conclude(attach_stdio(name, takeover))
}

/// `dshd-run probe NAME`: prints one `ProbeLine` as JSON on stdout. Exit 0,
/// or 5 when the socket is absent, 1 on I/O failure or timeout.
pub fn probe(name: &str) -> u8 {
    conclude(print_probe(name))
}

fn attach_stdio(name: &str, takeover: bool) -> Result<(), Error> {
    let socket = client_socket(name)?;
    match attach::handshake(&socket, name, &Handshake::Attach { takeover })? {
        (stream, HandshakeReply::Attached) => attach::pump_stdio(&stream),
        (_, reply) => Err(refused(name, &socket, &reply)),
    }
}

fn print_probe(name: &str) -> Result<(), Error> {
    let socket = client_socket(name)?;
    match attach::handshake(&socket, name, &Handshake::Probe)? {
        (_, HandshakeReply::Probe(line)) => out::json(&line),
        (_, reply) => Err(refused(name, &socket, &reply)),
    }
}

/// `$XDG_RUNTIME_DIR/dshd/NAME/acp.sock`.
fn client_socket(name: &str) -> Result<PathBuf, Error> {
    let name = InstanceName::parse(name)?;
    let root = std::env::var_os(RUNTIME_ROOT)
        .map(PathBuf::from)
        .filter(|root| root.is_absolute());
    let root = root.ok_or_else(|| {
        Error::SystemdUnavailable(format!("{RUNTIME_ROOT} is not set to an absolute path"))
    })?;
    Ok(RuntimeDir::under(&root, &name).acp_socket())
}

/// The error for a reply that is not the one asked for.
fn refused(name: &str, socket: &Path, reply: &HandshakeReply) -> Error {
    match reply {
        HandshakeReply::Refused {
            reason: Refusal::AttachedElsewhere,
        } => Error::AttachedElsewhere(name.to_owned()),
        HandshakeReply::Refused {
            reason: Refusal::ChildExited,
        } => Error::NotRunning(name.to_owned()),
        HandshakeReply::Refused {
            reason: Refusal::BadHandshake,
        }
        | HandshakeReply::Attached
        | HandshakeReply::Probe(_) => {
            let detail = io::Error::new(
                io::ErrorKind::InvalidData,
                "the relay answered outside the protocol",
            );
            Error::io("handshake with", socket, detail)
        }
    }
}

/// Reports a client failure on stderr and returns the exit status.
fn conclude(result: Result<(), Error>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(error) => out::fail(false, &error),
    }
}

/// Accepts connections until `shutdown` wakes it, or the listener fails.
fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener) {
    let mut listening = true;
    while listening {
        match listener.accept() {
            Ok(_) if shared.stopping.load(Ordering::SeqCst) => listening = false,
            Ok((stream, _peer)) => {
                let connection = Arc::clone(shared);
                if let Err(error) = spawn(CONNECTION_THREAD, move || serve(&connection, &stream)) {
                    log_line(&error.to_string());
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) => {}
            Err(error) => {
                log_line(&format!("the relay stopped accepting connections: {error}"));
                listening = false;
            }
        }
    }
}

/// One connection: the handshake, then the probe answer or the controller.
fn serve(shared: &Shared, stream: &UnixStream) {
    if let Err(error) = serve_connection(shared, stream) {
        log_line(&format!("a relay connection failed: {error}"));
    }
}

fn serve_connection(shared: &Shared, stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = match pump::next_frame(&mut reader)? {
        Frame::Line(line) => serde_json::from_slice::<Handshake>(&line).ok(),
        Frame::Oversize | Frame::End => None,
    };
    match request {
        Some(Handshake::Probe) => {
            let report = probe_line(&lock(&shared.state));
            write_reply(stream, &HandshakeReply::Probe(report))
        }
        Some(Handshake::Attach { takeover }) => attach_controller(shared, stream, reader, takeover),
        None => write_reply(
            stream,
            &HandshakeReply::Refused {
                reason: Refusal::BadHandshake,
            },
        ),
    }
}

/// Makes the connection the controller, unless one is attached and this is
/// no takeover, or dsh is gone; then routes its frames until it ends.
///
/// A refusal is decided under `state` alone and answered at once, so a
/// plain attach never waits on `out`, which a write to a stalled controller
/// holds for up to [`CONTROLLER_STALL`]. A takeover shuts the old
/// controller down first, which fails such a write and releases `out`.
fn attach_controller(
    shared: &Shared,
    stream: &UnixStream,
    reader: BufReader<UnixStream>,
    takeover: bool,
) -> io::Result<()> {
    {
        let state = lock(&shared.state);
        if let Some(reason) = refusal(&state, takeover) {
            drop(state);
            return write_reply(stream, &HandshakeReply::Refused { reason });
        }
        if takeover && let Some(attached) = &state.attached {
            pump::shut(&attached.closer);
        }
    }
    stream.set_write_timeout(Some(CONTROLLER_STALL))?;
    let writer = stream.try_clone()?;
    let closer = stream.try_clone()?;
    let mut out = lock(&shared.out);
    let mut state = lock(&shared.state);
    // Decided again: dsh may have gone, or another controller attached, meanwhile.
    if let Some(reason) = refusal(&state, takeover) {
        drop(state);
        drop(out);
        return write_reply(stream, &HandshakeReply::Refused { reason });
    }
    if let Some(previous) = out.take() {
        pump::shut(&previous.stream);
        log_line("a controller took over the relay");
    }
    state.held.unbind_all();
    state.generation += 1;
    let generation = state.generation;
    state.attached = Some(Attachment {
        generation,
        since: fsx::unix_now(),
        closer,
    });
    let stranded = pump::stranded_requests(&state);
    drop(state);
    if let Err(error) = write_reply(stream, &HandshakeReply::Attached) {
        pump::close_controller(shared, &mut out, generation);
        return Err(error);
    }
    log_line("a controller attached");
    *out = Some(Controller {
        generation,
        stream: writer,
    });
    pump::send(shared, &mut out, stranded);
    drop(out);
    stream.set_read_timeout(None)?;
    pump::controller_pump(shared, generation, reader);
    Ok(())
}

/// Why an attach is refused now, if it is.
fn refusal(state: &State, takeover: bool) -> Option<Refusal> {
    if state.exited {
        Some(Refusal::ChildExited)
    } else if state.attached.is_some() && !takeover {
        Some(Refusal::AttachedElsewhere)
    } else {
        None
    }
}

fn probe_line(state: &State) -> ProbeLine {
    ProbeLine {
        attached: state.attached.is_some(),
        attached_since: state.attached.as_ref().map(|attached| attached.since),
        initialized: state.initialized,
        held_sessions: state.held.report(),
        pending_permissions: state
            .waiting
            .iter()
            .filter(|request| request.permission)
            .count(),
    }
}

/// Writes one handshake reply line.
fn write_reply(stream: &UnixStream, reply: &HandshakeReply) -> io::Result<()> {
    let line = serde_json::to_vec(reply)?;
    let mut writer = stream;
    pump::put_frame(&mut writer, &line)
}

/// Locks a mutex; a thread that panicked holding it left consistent data,
/// since every update is a single assignment or collection operation.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn(
    name: &'static str,
    body: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, Error> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map_err(|source| Error::io("spawn thread", name, source))
}

fn join(handle: JoinHandle<()>, name: &str) {
    if handle.join().is_err() {
        log_line(&format!("the relay thread {name} panicked"));
    }
}
