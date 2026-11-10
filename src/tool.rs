//! Running external tools. Every `Command::new` in the crate goes through
//! here, so `PATH` lookup, the C locale and the `ENOENT` to `tool-missing`
//! mapping are decided once. The runner's starts of `dsh` itself are the
//! one other kind of process start, in `runner`, because they run with the
//! instance's environment and nothing else.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Output, Stdio};

use crate::error::{Error, ToolFailure};

/// A tool found on `PATH` (or overridden by an absolute program path, which
/// the checks use to substitute fakes).
#[derive(Debug, Clone)]
pub struct Tool {
    /// The tool's name, used in messages and for the error kind.
    pub name: &'static str,
    /// What to execute: the bare name, or an absolute path.
    pub program: OsString,
}

/// Captured stdout and stderr of a tool that exited 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captured {
    /// Raw stdout.
    pub stdout: Vec<u8>,
    /// Raw stderr.
    pub stderr: Vec<u8>,
}

impl Tool {
    /// A tool executed by name through `PATH`.
    pub fn named(name: &'static str) -> Self {
        Self {
            name,
            program: OsString::from(name),
        }
    }

    /// A tool executed from `program`; `name` still labels messages.
    pub fn at(name: &'static str, program: impl Into<OsString>) -> Self {
        Self {
            name,
            program: program.into(),
        }
    }

    /// Builds the command: C locale, no colour, stdin closed. Callers add
    /// arguments and environment.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.env("LC_ALL", "C")
            .env("SYSTEMD_COLORS", "0")
            .env("SYSTEMD_PAGER", "")
            .env("SYSTEMD_URLIFY", "0")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        cmd
    }

    /// Runs the tool with `args`, capturing both streams. Exit 0 yields the
    /// capture; any other exit is `Error::Tool`; `ENOENT` is
    /// `Error::ToolMissing`.
    pub fn run<I, S>(&self, args: I) -> Result<Captured, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let output = self
            .command()
            .args(&args)
            .output()
            .map_err(|e| self.spawn_error(e))?;
        self.check(&args, output)
    }

    /// Maps a spawn failure: `ENOENT` is the tool missing, anything else is
    /// an I/O failure on the program path.
    pub fn spawn_error(&self, source: io::Error) -> Error {
        if source.kind() == io::ErrorKind::NotFound {
            Error::ToolMissing(self.name)
        } else {
            Error::io("run", self.program.clone(), source)
        }
    }

    /// Turns a finished `Output` into the capture or the tool failure.
    pub fn check(&self, args: &[OsString], output: Output) -> Result<Captured, Error> {
        if output.status.success() {
            return Ok(Captured {
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        Err(Error::Tool(Box::new(ToolFailure {
            tool: self.name,
            args: args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })))
    }
}

impl Captured {
    /// stdout as text, lossily: for parsers of output that is ASCII by
    /// contract (`systemctl show`), and for journal and `nix` text shown to
    /// the user, where a byte that is not UTF-8 becomes U+FFFD.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned() // gate: allow R5 (display text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: a missing executable is reported as a generic tool failure instead of naming the absent required tool
    #[test]
    fn missing_program_is_tool_missing() {
        let tool = Tool::at("systemctl", "/nonexistent/dshd-no-such-tool");
        match tool.run(["--version"]) {
            Err(Error::ToolMissing("systemctl")) => {}
            other => panic!("{other:?}"),
        }
    }

    // catches: a failed external command is treated as success or loses its exit status, stderr or error kind
    #[test]
    fn nonzero_exit_is_tool_failure() {
        let tool = Tool::at("journalctl", "/bin/sh");
        match tool.run(["-c", "echo bad >&2; exit 3"]) {
            Err(Error::Tool(f)) => {
                assert_eq!(f.status, Some(3));
                assert_eq!(f.stderr, "bad");
                assert_eq!(Error::Tool(f).kind(), "io");
            }
            other => panic!("{other:?}"),
        }
    }
}
