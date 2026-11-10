//! Independent newline-delimited JSON helpers for the relay integration controller.

use std::io::{self, BufRead, Write};

use serde_json::Value;

pub(crate) fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    if reader.read_until(b'\n', &mut frame)? == 0 {
        return Ok(None);
    }
    if frame.last() == Some(&b'\n') {
        frame.pop();
    }
    Ok(Some(frame))
}

pub(crate) fn write_frame(writer: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    writer.write_all(frame)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(crate) fn method_of(frame: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(frame)
        .ok()?
        .get("method")?
        .as_str()
        .map(str::to_owned)
}

pub(crate) fn session_id_of(frame: &[u8]) -> Option<String> {
    let frame: Value = serde_json::from_slice(frame).ok()?;
    frame
        .pointer("/params/sessionId")
        .or_else(|| frame.pointer("/result/sessionId"))?
        .as_str()
        .map(str::to_owned)
}
