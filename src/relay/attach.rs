//! The client side: connect, handshake, then pump stdin to the socket and
//! the socket to stdout until either side ends. Used by `dshd-run attach`
//! and `dshd-run probe`.
//!
//! The client frames nothing after the handshake: bytes go through as they
//! come, and the relay does the framing.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;

use crate::error::Error;
use crate::out;
use crate::relay::protocol::{Handshake, HandshakeReply};
use crate::relay::{FRAME_CAP, PROBE_TIMEOUT};

/// Bytes moved per read from the relay.
const CHUNK: usize = 64 * 1024;
/// The thread that forwards stdin.
const STDIN_THREAD: &str = "dshd-attach-stdin";
/// How errors name the connection once the handshake is done.
const RELAY: &str = "the relay socket";

/// Connects and exchanges the handshake line. A missing socket is
/// `NotRunning`.
pub fn handshake(
    socket: &Path,
    name: &str,
    request: &Handshake,
) -> Result<(UnixStream, HandshakeReply), Error> {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Error::NotRunning(name.to_owned()));
        }
        Err(source) => return Err(Error::io("connect to", socket, source)),
    };
    match exchange(&stream, request) {
        Ok(reply) => Ok((stream, reply)),
        Err(source) => Err(Error::io("handshake with", socket, source)),
    }
}

/// Pumps the process stdio through an attached stream. Returns when the
/// relay closes the stream or stdin reaches EOF.
pub fn pump_stdio(stream: &UnixStream) -> Result<(), Error> {
    let writer = stream
        .try_clone()
        .map_err(|source| Error::io("open", RELAY, source))?;
    thread::Builder::new()
        .name(STDIN_THREAD.to_owned())
        .spawn(move || forward_stdin(&writer))
        .map(|_detached| ())
        .map_err(|source| Error::io("spawn thread", STDIN_THREAD, source))?;
    let mut reader = stream;
    let mut chunk = vec![0_u8; CHUNK];
    let mut open = true;
    while open {
        match reader.read(&mut chunk) {
            Ok(read) => match chunk.get(..read) {
                Some(bytes) if !bytes.is_empty() => out::write(bytes)?,
                Some(_) | None => open = false,
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(Error::io("read from", RELAY, source)),
        }
    }
    Ok(())
}

/// Sends the request line and reads the reply line, within `PROBE_TIMEOUT`.
fn exchange(stream: &UnixStream, request: &Handshake) -> io::Result<HandshakeReply> {
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    let mut writer = stream;
    writer.write_all(&line)?;
    let reply = read_line(stream)?;
    stream.set_read_timeout(None)?;
    serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Reads one line a byte at a time, so nothing the relay sends after it is
/// taken from the stream.
fn read_line(stream: &UnixStream) -> io::Result<Vec<u8>> {
    let mut reader = stream;
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while line.len() <= FRAME_CAP {
        reader.read_exact(&mut byte)?;
        if byte == *b"\n" {
            return Ok(line);
        }
        line.extend_from_slice(&byte);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the relay's reply has no end",
    ))
}

/// Copies stdin to the relay; at its end, tells the relay with a half
/// close, which detaches this controller.
fn forward_stdin(relay: &UnixStream) {
    let mut writer = relay;
    if let Err(error) = io::copy(&mut io::stdin().lock(), &mut writer)
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        out::error_line(&format!("dshd: cannot forward stdin to the relay: {error}"));
    }
    if let Err(error) = relay.shutdown(Shutdown::Write)
        && error.kind() != io::ErrorKind::NotConnected
    {
        out::error_line(&format!("dshd: cannot end the input to the relay: {error}"));
    }
}
