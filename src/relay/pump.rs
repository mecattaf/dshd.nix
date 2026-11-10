//! Framing and pumping: newline-delimited JSON-RPC frames up to
//! `FRAME_CAP`, the child-to-controller and controller-to-child threads,
//! and the inspection that keeps `Held` current (`session/new`,
//! `session/resume`, `session/close`, `session/request_permission`).
//!
//! Frames travel as bytes. The relay parses a frame only to route it,
//! re-encodes one only to swap a controller's request id for its own and
//! back, and writes three kinds of its own: the answer to a resume of a
//! held session, a JSON-RPC error, and the `cancelled` answer to a pending
//! permission when the instance stops.

mod routing;

use routing::{from_child, from_controller, reply};

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::ChildStdout;

use serde_json::{Map, Value};

use crate::relay::protocol::{
    METHOD_INITIALIZE, METHOD_REQUEST_PERMISSION, METHOD_SESSION_CLOSE, METHOD_SESSION_LIST,
    METHOD_SESSION_NEW, METHOD_SESSION_RESUME,
};
use crate::relay::{
    CONTROLLER_STALL, ChildRequest, Controller, FRAME_CAP, InFlight, Shared, State, Tracked, lock,
};
use crate::runner::log_line;

/// JSON-RPC: the frame is not JSON.
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC: the frame is not a request, a notification or a response.
const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC: a resume of a held session with other parameters.
const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC: dsh is gone, so the request will not be answered.
const INTERNAL_ERROR: i64 = -32603;
/// The message of [`INTERNAL_ERROR`].
const AGENT_EXITED: &str = "agent exited";
/// What one frame read may consume: the cap and the newline.
const LINE_LIMIT: u64 = FRAME_CAP as u64 + 1;
/// One read from a frame stream.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Frame {
    /// A line without its newline.
    Line(Vec<u8>),
    /// A line longer than `FRAME_CAP`, skipped up to its newline.
    Oversize,
    /// The end of the stream.
    End,
}

/// A frame on its way to the controller, and the held session whose ring
/// takes it back when the controller cannot.
#[derive(Debug)]
pub(super) struct Outgoing {
    frame: Vec<u8>,
    ring: Option<String>,
}

impl Outgoing {
    fn live(frame: Vec<u8>) -> Self {
        Self { frame, ring: None }
    }
}

/// Reads the next line that is not blank. A line past `FRAME_CAP` is
/// skipped and reported; a last line without a newline still counts.
pub(super) fn next_frame<R: BufRead>(reader: &mut R) -> io::Result<Frame> {
    let mut line = Vec::new();
    while line.is_empty() {
        if reader
            .by_ref()
            .take(LINE_LIMIT)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(Frame::End);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > FRAME_CAP {
            reader.skip_until(b'\n')?;
            return Ok(Frame::Oversize);
        }
        if line.trim_ascii().is_empty() {
            line.clear();
        }
    }
    Ok(Frame::Line(line))
}

/// Writes `frame` and a newline in one write, then flushes.
pub(super) fn put_frame<W: Write>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    let mut line = Vec::with_capacity(frame.len() + 1);
    line.extend_from_slice(frame);
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// The child-to-controller pump: routes every frame dsh writes until its
/// stdout ends, then winds the relay down.
pub(super) fn child_pump(shared: &Shared, stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut reading = true;
    while reading {
        match next_frame(&mut reader) {
            Ok(Frame::Line(frame)) => from_child(shared, frame),
            Ok(Frame::Oversize) => log_line(&format!(
                "dropped a frame from dsh larger than {FRAME_CAP} bytes"
            )),
            Ok(Frame::End) => reading = false,
            Err(error) => {
                log_line(&format!("stopped reading dsh's output: {error}"));
                reading = false;
            }
        }
    }
    wind_down(shared);
    lock(&shared.state).closed = true;
    shared.closed.notify_all();
}

/// The controller-to-child pump of the controller of `generation`, until
/// its connection ends; then it is detached. Its sessions stay held.
pub(super) fn controller_pump(shared: &Shared, generation: u64, mut reader: BufReader<UnixStream>) {
    let mut reading = true;
    while reading {
        match next_frame(&mut reader) {
            Ok(Frame::Line(frame)) => from_controller(shared, generation, &frame),
            Ok(Frame::Oversize) => {
                let detail = format!("a frame is larger than {FRAME_CAP} bytes");
                reply(
                    shared,
                    generation,
                    error_frame(&Value::Null, INVALID_REQUEST, &detail),
                );
            }
            Ok(Frame::End) => reading = false,
            Err(error) => {
                log_line(&format!("the controller connection failed: {error}"));
                reading = false;
            }
        }
    }
    let mut out = lock(&shared.out);
    close_controller(shared, &mut out, generation);
}

/// Writes frames to the attached controller. A failed write, or one that
/// made no progress for [`CONTROLLER_STALL`], detaches it on the spot; the
/// frames of held sessions it did not get go back to their rings.
pub(super) fn send(
    shared: &Shared,
    out: &mut Option<Controller>,
    frames: impl IntoIterator<Item = Outgoing>,
) {
    let Some(controller) = out.as_mut() else {
        return;
    };
    let mut frames: Vec<Outgoing> = frames.into_iter().collect();
    let failed =
        frames.iter().position(
            |item| match put_frame(&mut controller.stream, &item.frame) {
                Ok(()) => false,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    log_line(&format!(
                        "the controller read nothing for {} s; detaching it",
                        CONTROLLER_STALL.as_secs()
                    ));
                    true
                }
                Err(error) => {
                    log_line(&format!(
                        "the controller stopped reading ({error}); detaching it"
                    ));
                    true
                }
            },
        );
    if let Some(index) = failed {
        let generation = controller.generation;
        let unsent = frames.split_off(index);
        close_controller(shared, out, generation);
        let mut state = lock(&shared.state);
        for item in unsent {
            if let Some(session) = item.ring {
                state.held.buffer(&session, item.frame);
            }
        }
    }
}

/// Closes the controller of `generation`, if it is still the one, and
/// unbinds every session. Called with `out` locked.
pub(super) fn close_controller(shared: &Shared, out: &mut Option<Controller>, generation: u64) {
    if let Some(controller) = out.take_if(|controller| controller.generation == generation) {
        shut(&controller.stream);
    }
    let mut state = lock(&shared.state);
    if state
        .attached
        .as_ref()
        .is_some_and(|attached| attached.generation == generation)
    {
        state.attached = None;
        state.held.unbind_all();
        log_line("the controller detached");
    }
}

/// Shuts a controller connection down both ways.
pub(super) fn shut(stream: &UnixStream) {
    if let Err(error) = stream.shutdown(Shutdown::Both)
        && error.kind() != io::ErrorKind::NotConnected
    {
        log_line(&format!("cannot close a controller connection: {error}"));
    }
}

/// dsh is gone or being stopped: the attached controller's requests in
/// flight get -32603 "agent exited", dsh's own pending requests are
/// forgotten, attaches are refused from now on, and the controller is
/// closed.
pub(super) fn wind_down(shared: &Shared) {
    let mut out = lock(&shared.out);
    let (generation, failed) = {
        let mut state = lock(&shared.state);
        state.exited = true;
        state.waiting.clear();
        let generation = state.attached.as_ref().map(|attached| attached.generation);
        let failed: Vec<Outgoing> = mem::take(&mut state.in_flight)
            .into_values()
            .filter(|flight| Some(flight.generation) == generation)
            .map(|flight| Outgoing::live(error_frame(&flight.id, INTERNAL_ERROR, AGENT_EXITED)))
            .collect();
        (generation, failed)
    };
    send(shared, &mut out, failed);
    if let Some(generation) = generation {
        close_controller(shared, &mut out, generation);
    }
}

/// Answers every pending permission request `cancelled`, so dsh ends its
/// turns before its stdin closes.
pub(super) fn cancel_permissions(shared: &Shared, stdin: &mut impl Write) -> bool {
    let permissions = {
        let mut state = lock(&shared.state);
        let (permissions, others): (Vec<ChildRequest>, Vec<ChildRequest>) =
            mem::take(&mut state.waiting)
                .into_iter()
                .partition(|request| request.permission);
        state.waiting = others;
        permissions
    };
    for request in permissions {
        if let Err(error) = put_frame(stdin, &cancelled_frame(&request.id)) {
            log_line(&format!(
                "cannot cancel dsh's pending permission without blocking: {error}"
            ));
            return false;
        }
    }
    true
}

/// dsh's pending requests that no held session will re-send on a resume,
/// for a controller that just attached.
pub(super) fn stranded_requests(state: &State) -> Vec<Outgoing> {
    state
        .waiting
        .iter()
        .filter(|request| {
            request
                .session
                .as_deref()
                .is_none_or(|session| !state.held.holds(session))
        })
        .map(|request| Outgoing::live(request.frame.clone()))
        .collect()
}

fn object(frame: &[u8]) -> Option<Map<String, Value>> {
    serde_json::from_slice(frame).ok()
}

fn session_of(message: &Map<String, Value>) -> Option<String> {
    ["params", "result"]
        .into_iter()
        .find_map(|key| message.get(key)?.get("sessionId")?.as_str())
        .map(str::to_owned)
}

fn text_param(params: Option<&Value>, key: &str) -> String {
    params
        .and_then(|params| params.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

/// `mcpServers` of a request; ACP treats a missing list as an empty one.
fn mcp_servers(params: Option<&Value>) -> Value {
    params
        .and_then(|params| params.get("mcpServers"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

/// Two cwds name the same directory: by real path when both resolve,
/// else as written.
fn same_dir(left: &str, right: &str) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn error_frame(id: &Value, code: i64, message: &str) -> Vec<u8> {
    let message = Value::String(message.to_owned());
    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{message}}}}}"#)
        .into_bytes()
}

fn cancelled_frame(id: &Value) -> Vec<u8> {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"outcome":{{"outcome":"cancelled"}}}}}}"#)
        .into_bytes()
}

/// The relay's answer to the resume of a held session. `dropped` frames
/// that never reached a controller are announced in `_meta`, so the client
/// can reload the session from dsh rather than take the replay as whole.
fn resumed_frame(id: &Value, config_options: Option<&Value>, dropped: usize) -> Vec<u8> {
    let mut result = Vec::new();
    if let Some(options) = config_options {
        result.push(format!(r#""configOptions":{options}"#));
    }
    if dropped > 0 {
        result.push(format!(
            r#""_meta":{{"dshd":{{"droppedFrames":{dropped}}}}}"#
        ));
    }
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{{}}}}}"#,
        result.join(",")
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::relay::FRAME_CAP;

    // catches: frames split or joined at the wrong byte, blank lines passed on, or a frame past 64 MiB read whole instead of skipped (P1.4)
    #[test]
    fn frames() -> std::io::Result<()> {
        let mut input = b"{\"a\":1}\n\n  \n{\"b\":2}\n".to_vec();
        input.extend(vec![b'x'; FRAME_CAP + 1]);
        input.extend_from_slice(b"\n");
        input.extend(vec![b'y'; FRAME_CAP]);
        input.extend_from_slice(b"\n{\"c\":3}");
        let mut reader = Cursor::new(input);
        assert_eq!(
            super::next_frame(&mut reader)?,
            super::Frame::Line(b"{\"a\":1}".to_vec())
        );
        assert_eq!(
            super::next_frame(&mut reader)?,
            super::Frame::Line(b"{\"b\":2}".to_vec())
        );
        assert!(matches!(
            super::next_frame(&mut reader),
            Ok(super::Frame::Oversize)
        ));
        assert_eq!(
            match super::next_frame(&mut reader)? {
                super::Frame::Line(frame) => Some(frame.len()),
                _ => None,
            },
            Some(FRAME_CAP),
            "exactly the cap is a frame"
        );
        assert_eq!(
            super::next_frame(&mut reader)?,
            super::Frame::Line(b"{\"c\":3}".to_vec()),
            "a last line without a newline counts"
        );
        assert_eq!(super::next_frame(&mut reader)?, super::Frame::End);

        let mut oversize = Cursor::new(vec![b'z'; FRAME_CAP + 1]);
        assert_eq!(super::next_frame(&mut oversize)?, super::Frame::Oversize);

        let mut written = Vec::new();
        super::put_frame(&mut written, b"{}")?;
        super::put_frame(&mut written, b"[]")?;
        assert_eq!(written, b"{}\n[]\n");
        Ok(())
    }

    // catches: routing on a nested "method" or "sessionId", or a response taken for a request (P3.1)
    #[test]
    fn method_and_session_id() {
        let method = |frame: &[u8]| {
            super::object(frame).and_then(|message| {
                message
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
        };
        let session =
            |frame: &[u8]| super::object(frame).and_then(|message| super::session_of(&message));
        let request = br#"{"jsonrpc":"2.0","id":7,"method":"session/prompt","params":{"sessionId":"s-1","prompt":[{"method":"inner"}]}}"#;
        assert_eq!(method(request).as_deref(), Some("session/prompt"));
        assert_eq!(session(request).as_deref(), Some("s-1"));
        let response = br#"{"jsonrpc":"2.0","id":7,"result":{"sessionId":"s-2"}}"#;
        assert_eq!(method(response), None);
        assert_eq!(session(response).as_deref(), Some("s-2"));
        let nested = br#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionId":"deep"}}}"#;
        assert_eq!(method(nested).as_deref(), Some("session/update"));
        assert_eq!(session(nested), None);
        assert_eq!(super::object(b"not json"), None);
        assert_eq!(super::object(b"[{\"sessionId\":\"x\"}]"), None);
    }
}
