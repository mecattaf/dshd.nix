//! stdout and stderr, the only place that writes them. A closed stdout
//! (`BrokenPipe`) is a quiet exit 0. Colour is used on a TTY with
//! `NO_COLOR` unset, never otherwise.

use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use serde::Serialize;

use crate::cli::report::ErrorJson;
use crate::error::Error;

/// Colour roles of the human output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Bold green: an active instance name.
    Active,
    /// Yellow: an inactive instance name.
    Inactive,
    /// Red: a failed instance name.
    Failed,
    /// Bold cyan: a command the user can run.
    Command,
}

/// The escape sequence that ends a styled span.
const RESET: &str = "\x1b[0m";

/// Writes `text` and a newline to stdout.
pub fn line(text: &str) -> Result<(), Error> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(b'\n');
    write(&bytes)
}

/// Writes raw bytes to stdout.
pub fn write(bytes: &[u8]) -> Result<(), Error> {
    write_to(&mut io::stdout().lock(), bytes)
}

/// Writes one JSON object and a newline to stdout.
pub fn json<T: Serialize>(value: &T) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| Error::Output(io::Error::from(e)))?;
    bytes.push(b'\n');
    write(&bytes)
}

/// Writes `text` and a newline to stderr. Failures are ignored.
pub fn error_line(text: &str) {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(b'\n');
    // stderr is where failures are reported; when it is closed nothing is left to tell.
    drop(io::stderr().lock().write_all(&bytes));
}

/// Reports an error on stderr, human (`dshd: MESSAGE` then the hint) or
/// JSON (`{"error":{...}}`), and returns its exit code.
pub fn fail(json: bool, error: &Error) -> u8 {
    for text in failure_lines(json, error) {
        error_line(&text);
    }
    error.exit_code()
}

/// Wraps `text` in the style's escape codes when colour is on.
pub fn styled(style: Style, text: &str) -> String {
    paint(style, text, colour_enabled())
}

/// Whether stdout is a TTY and `NO_COLOR` is unset.
fn colour_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        stdout_is_terminal() && env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
    })
}

/// Whether stdout is a TTY, read once per process. It decides the colour
/// and the default output format.
pub fn stdout_is_terminal() -> bool {
    static TERMINAL: OnceLock<bool> = OnceLock::new();
    *TERMINAL.get_or_init(|| io::stdout().is_terminal())
}

/// The bytes go out in one `write_all` and are flushed, so a closed reader
/// shows up here as `Error::Output` with the OS error kind intact.
fn write_to(out: &mut impl Write, bytes: &[u8]) -> Result<(), Error> {
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(Error::Output)
}

/// The stderr lines for `error`: none for a closed stdout, one JSON object
/// in JSON, else the message and, when there is one, the remedy.
fn failure_lines(json: bool, error: &Error) -> Vec<String> {
    if matches!(error, Error::Output(source) if source.kind() == io::ErrorKind::BrokenPipe) {
        return Vec::new();
    }
    let human = format!("dshd: {error}");
    if json {
        return match serde_json::to_string(&ErrorJson::new(error)) {
            Ok(object) => vec![object],
            Err(_) => vec![human],
        };
    }
    let mut lines = vec![human];
    if let Some(hint) = error.hint() {
        lines.push(format!("  remedy: {hint}"));
    }
    lines
}

/// `text` in the style's escape codes when `on`, verbatim otherwise.
fn paint(style: Style, text: &str, on: bool) -> String {
    if !on {
        return text.to_owned();
    }
    let start = match style {
        Style::Active => "\x1b[1;32m",
        Style::Inactive => "\x1b[33m",
        Style::Failed => "\x1b[31m",
        Style::Command => "\x1b[1;36m",
    };
    format!("{start}{text}{RESET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: a closed stdout reported as an error or a non-zero exit (`dshd ls --text | head -1`)
    #[test]
    fn broken_pipe_is_zero() {
        let (reader, mut writer) = io::pipe().unwrap();
        drop(reader);
        let error = write_to(&mut writer, b"web-1: running(dsh)\n").unwrap_err();
        assert!(
            matches!(&error, Error::Output(source) if source.kind() == io::ErrorKind::BrokenPipe)
        );
        assert_eq!(error.exit_code(), 0);
        assert!(failure_lines(false, &error).is_empty());
        assert!(failure_lines(true, &error).is_empty());
        assert_eq!(fail(false, &error), 0);
    }

    // catches: the JSON error shape or the remedy line drifting from the error's kind, message and hint
    #[test]
    fn fail_renders_human_and_json() {
        let error = Error::NotFound("web-9".into());
        assert_eq!(
            failure_lines(false, &error),
            vec!["dshd: no instance named web-9", "  remedy: dshd ls"]
        );
        let lines = failure_lines(true, &error);
        assert_eq!(lines.len(), 1);
        let object: serde_json::Value = serde_json::from_str(lines.first().unwrap()).unwrap();
        assert_eq!(object.pointer("/error/kind").unwrap(), "not-found");
        assert_eq!(
            object.pointer("/error/message").unwrap(),
            "no instance named web-9"
        );
        assert_eq!(object.pointer("/error/hint").unwrap(), "dshd ls");
        let silent = Error::io("read", "/srv/x", io::Error::from(io::ErrorKind::Other));
        assert_eq!(
            failure_lines(false, &silent).len(),
            1,
            "no remedy line without a hint"
        );
        let lines = failure_lines(true, &silent);
        let parsed: serde_json::Value = serde_json::from_str(lines.first().unwrap()).unwrap();
        assert!(parsed.pointer("/error/hint").unwrap().is_null());
    }

    // catches: escape codes leaking into piped output, or the wrong colour for a role
    #[test]
    fn paint_wraps_only_when_on() {
        assert_eq!(paint(Style::Command, "dshd start x", false), "dshd start x");
        assert_eq!(paint(Style::Active, "x", true), "\x1b[1;32mx\x1b[0m");
        assert_eq!(paint(Style::Inactive, "x", true), "\x1b[33mx\x1b[0m");
        assert_eq!(paint(Style::Failed, "x", true), "\x1b[31mx\x1b[0m");
        assert_eq!(paint(Style::Command, "x", true), "\x1b[1;36mx\x1b[0m");
    }
}
