//! The command-runner abstraction the launchd backend drives `launchctl`
//! through. The real runner is a [`Tool`] found on `PATH`; the unit tests
//! substitute an in-process fake that models launchd's jobs
//! (`launchd::fake`), so every verb's exact `launchctl` argv is checked on
//! any platform.

use std::fmt;

use crate::error::Error;
use crate::tool::{Captured, Tool};

/// Runs one `launchctl` command. Exit 0 yields the capture; any other exit
/// is `Error::Tool` with the exit status and trimmed stderr, exactly as
/// [`Tool::run`] reports it; a missing program is `Error::ToolMissing`.
pub trait CommandRunner: fmt::Debug {
    /// The tool's name, as messages and error kinds name it.
    fn name(&self) -> &'static str;

    /// Runs the tool with `args` (without the program name).
    fn run(&self, args: &[String]) -> Result<Captured, Error>;
}

impl CommandRunner for Tool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&self, args: &[String]) -> Result<Captured, Error> {
        Tool::run(self, args)
    }
}
