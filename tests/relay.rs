//! The ACP relay through its real client, `dshd-run attach`, against a
//! runner and the stub dsh: one controller at a time, takeover, the buffer
//! a detached session keeps and the hole a resume announces, the resume
//! the relay answers itself, pending permissions across controllers, a stop
//! that lets dsh answer first, a controller that stops reading, and frames
//! the relay refuses.

mod common;
#[path = "relay/wire.rs"]
mod wire;

use wire as pump;

#[path = "relay/backpressure.rs"]
mod backpressure;
#[path = "relay/handover.rs"]
mod handover;
#[path = "relay/protocol.rs"]
mod protocol;
#[path = "relay/stop.rs"]
mod stop;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use common::{Instance, Running, Spec, TestResult, wait_for};
use dshd::relay::RING_PER_SESSION;
use dshd::relay::protocol::HandshakeReply;
use serde_json::{Value, json};

/// How long a client waits for one frame before the test fails.
const FRAME_BOUND: Duration = Duration::from_secs(20);
/// The chunks the stub writes for a prompt that mentions "flood".
const FLOOD: usize = 4000;

/// A controller: `dshd-run attach NAME`, its stdin for frames to the
/// relay and a thread turning its stdout back into frames.
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    frames: Receiver<Value>,
}

impl Client {
    fn attach(instance: &Instance, takeover: bool) -> TestResult<Self> {
        let mut command = instance.runner("attach");
        if takeover {
            command.arg("--takeover");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = child.stdout.take().ok_or("attach has no stdout")?;
        let input = child.stdin.take();
        let (sender, frames) = mpsc::channel();
        // Detached: it ends with the client's stdout.
        drop(thread::spawn(move || read_frames(output, &sender)));
        Ok(Self {
            child,
            input,
            frames,
        })
    }

    fn send(&mut self, frame: &Value) -> TestResult {
        self.send_raw(&serde_json::to_vec(frame)?)
    }

    fn send_raw(&mut self, bytes: &[u8]) -> TestResult {
        let input = self.input.as_mut().ok_or("the client's stdin is closed")?;
        pump::write_frame(input, bytes)?;
        Ok(())
    }

    fn receive(&self) -> TestResult<Value> {
        Ok(self.frames.recv_timeout(FRAME_BOUND)?)
    }

    /// Whether the relay ended the connection with no frame left to read.
    fn closed(&self) -> bool {
        matches!(
            self.frames.recv_timeout(FRAME_BOUND),
            Err(RecvTimeoutError::Disconnected)
        )
    }

    /// Frames until one satisfies `wanted`: that one, and those before it.
    fn until(&self, wanted: impl Fn(&Value) -> bool) -> TestResult<(Value, Vec<Value>)> {
        let mut before = Vec::new();
        let mut frame = self.receive()?;
        while !wanted(&frame) {
            before.push(frame);
            frame = self.receive()?;
        }
        Ok((frame, before))
    }

    /// A request and its answer, with the frames that came first.
    fn call(&mut self, id: u64, method: &str, params: &Value) -> TestResult<(Value, Vec<Value>)> {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        self.until(|frame| method_of(frame).is_none() && frame.get("id") == Some(&json!(id)))
    }

    /// Ends stdin, which detaches the controller, and waits for the client.
    fn detach(mut self) -> TestResult<Option<i32>> {
        drop(self.input.take());
        self.exit()
    }

    /// Kills the client, as a controller dies mid-turn.
    fn kill(mut self) -> TestResult {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }

    /// The exit code once the client has exited on its own.
    fn exit(&mut self) -> TestResult<Option<i32>> {
        let mut status = None;
        wait_for("the client to exit", || {
            status = self.child.try_wait()?;
            Ok(status.is_some())
        })?;
        Ok(status.and_then(|status| status.code()))
    }

    /// The exit code and stderr of a client the relay turned away.
    fn refused(mut self) -> TestResult<(Option<i32>, String)> {
        let code = self.exit()?;
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_string(&mut stderr)?;
        }
        Ok((code, stderr))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) && self.child.kill().is_ok() {
            drop(self.child.wait());
        }
    }
}

fn read_frames(output: ChildStdout, sender: &Sender<Value>) {
    let mut reader = BufReader::new(output);
    while let Ok(Some(frame)) = pump::read_frame(&mut reader) {
        let value = serde_json::from_slice(&frame)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&frame).into_owned()));
        if sender.send(value).is_err() {
            return;
        }
    }
}

/// An ACP instance with its runner started and the relay listening.
fn acp_instance(name: &str) -> TestResult<(Instance, Running)> {
    let instance = Instance::create(
        name,
        &Spec {
            profile: "acp",
            args: &[],
            env: &[],
        },
    )?;
    let socket = instance.runtime().acp_socket();
    let running = instance.start()?;
    wait_for("the relay socket", || Ok(socket.exists()))?;
    Ok((instance, running))
}

fn probe(instance: &Instance) -> TestResult<dshd::relay::protocol::ProbeLine> {
    let output = instance.runner("probe").output()?;
    if output.status.code() != Some(0) {
        return Err(format!("probe failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// The probe's row for a held session, from its wire shape.
fn row(
    session: &str,
    cwd: &str,
    bound: bool,
    buffered: usize,
) -> TestResult<dshd::relay::protocol::HeldSession> {
    let wire = json!({ "sessionId": session, "cwd": cwd, "bound": bound, "buffered": buffered, "dropped": 0 });
    Ok(serde_json::from_value(wire)?)
}

fn cwd_of(instance: &Instance, relative: &str) -> TestResult<String> {
    Ok(instance
        .path(relative)
        .to_str()
        .ok_or("a scratch path that is not UTF-8")?
        .to_owned())
}

/// The method of a frame, as the relay itself reads it.
fn method_of(frame: &Value) -> Option<String> {
    pump::method_of(frame.to_string().as_bytes())
}

/// The text of an `agent_message_chunk` update.
fn text_of(frame: &Value) -> Option<&str> {
    frame
        .pointer("/params/update/content/text")
        .and_then(Value::as_str)
}

fn initialize(client: &mut Client, id: u64) -> TestResult<Value> {
    Ok(client
        .call(
            id,
            "initialize",
            &json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        )?
        .0)
}

/// `session/new` in `cwd`; the new session's id.
fn open(client: &mut Client, id: u64, cwd: &str) -> TestResult<String> {
    let (opened, _) = client.call(id, "session/new", &json!({ "cwd": cwd, "mcpServers": [] }))?;
    let session = opened
        .pointer("/result/sessionId")
        .and_then(Value::as_str)
        .ok_or("session/new gave no session")?;
    Ok(session.to_owned())
}

fn prompt(id: u64, session: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": { "sessionId": session, "prompt": [{ "type": "text", "text": text }] }
    })
}

fn resume(
    client: &mut Client,
    id: u64,
    session: &str,
    cwd: &str,
    servers: &Value,
) -> TestResult<Value> {
    Ok(client
        .call(
            id,
            "session/resume",
            &json!({ "sessionId": session, "cwd": cwd, "mcpServers": servers }),
        )?
        .0)
}

/// A request on a raw controller connection and its answer, skipping what
/// came first.
fn raw_call(
    stream: &UnixStream,
    reader: &mut impl BufRead,
    id: u64,
    method: &str,
    params: &Value,
) -> TestResult<Value> {
    let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let mut writer = stream;
    pump::write_frame(&mut writer, request.to_string().as_bytes())?;
    loop {
        let frame: Value = serde_json::from_slice(
            &pump::read_frame(reader)?.ok_or("the relay closed the connection")?,
        )?;
        if method_of(&frame).is_none() && frame.get("id") == Some(&json!(id)) {
            return Ok(frame);
        }
    }
}

/// The methods the stub received, in order.
fn methods(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|line| line.strip_prefix("acp ")?.split(' ').nth(1))
        .filter(|method| *method != "response")
        .collect()
}

/// The stub's record of permission answers, as `ID OUTCOME`.
fn answers(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|line| {
            line.strip_prefix("acp ")?
                .split_once(' ')?
                .1
                .strip_prefix("response ")
        })
        .collect()
}

fn asks_permission(frame: &Value) -> bool {
    method_of(frame).as_deref() == Some(dshd::relay::protocol::METHOD_REQUEST_PERMISSION)
}
