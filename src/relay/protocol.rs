//! The relay's wire types: the one-line handshake and its reply, the probe
//! report, and the handful of ACP method names the relay recognises.
//! `ProbeLine` is also the `acp` block of the CLI's status object.

use serde::{Deserialize, Serialize};

/// First line a client sends after connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dshd", rename_all = "kebab-case")]
pub enum Handshake {
    /// Become the controller.
    Attach {
        /// Replace an attached controller.
        #[serde(default)]
        takeover: bool,
    },
    /// Ask for a report and disconnect.
    Probe,
}

/// Why an attach was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// Another controller is attached and `takeover` was false.
    AttachedElsewhere,
    /// The first line was not a handshake.
    BadHandshake,
    /// The child has exited.
    ChildExited,
}

/// First line the relay sends back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dshd", rename_all = "kebab-case")]
pub enum HandshakeReply {
    /// The client is now the controller.
    Attached,
    /// The client is not.
    Refused {
        /// Why.
        reason: Refusal,
    },
    /// The probe report.
    Probe(ProbeLine),
}

/// One held session as the relay remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldSession {
    /// DSH's session id.
    pub session_id: String,
    /// The cwd it was created with.
    pub cwd: String,
    /// A controller currently has it resumed.
    pub bound: bool,
    /// Frames buffered for it.
    pub buffered: usize,
    /// Frames dropped from its buffer since it was held.
    pub dropped: usize,
}

/// The relay's report, also the `acp` block of `dshd get --json`.
///
/// The installed CLI reads this line from the booted runner of any older
/// release, so a field added later is `#[serde(default)]` and its absence
/// means the earlier report, as for the records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeLine {
    /// A controller is attached.
    pub attached: bool,
    /// Unix seconds the controller attached, when one is.
    pub attached_since: Option<u64>,
    /// The ACP `initialize` exchange with the child completed.
    pub initialized: bool,
    /// Sessions the relay holds.
    pub held_sessions: Vec<HeldSession>,
    /// Permission requests from the child waiting for a controller.
    pub pending_permissions: usize,
}

/// ACP methods the relay inspects; every other frame is forwarded as is.
pub const METHOD_INITIALIZE: &str = "initialize";
/// `session/new`.
pub const METHOD_SESSION_NEW: &str = "session/new";
/// `session/list`.
pub const METHOD_SESSION_LIST: &str = "session/list";
/// `session/resume`.
pub const METHOD_SESSION_RESUME: &str = "session/resume";
/// `session/close`.
pub const METHOD_SESSION_CLOSE: &str = "session/close";
/// `session/request_permission`.
pub const METHOD_REQUEST_PERMISSION: &str = "session/request_permission";

#[cfg(test)]
mod tests {
    use super::{HeldSession, ProbeLine};

    // catches: a probe field renamed, retyped or made required, so the CLI can no longer read the line a runner of
    // the first published dshd prints
    #[test]
    fn first_published_probe_line_decodes() -> Result<(), serde_json::Error> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/probe.json"
        ));
        let line: ProbeLine = serde_json::from_str(fixture)?;
        let held = HeldSession {
            session_id: "stub-1".to_owned(),
            cwd: "/srv/work/project".to_owned(),
            bound: true,
            buffered: 0,
            dropped: 0,
        };
        let expected = ProbeLine {
            attached: true,
            attached_since: Some(1_794_009_700),
            initialized: true,
            held_sessions: vec![held],
            pending_permissions: 1,
        };
        assert_eq!(line, expected);
        Ok(())
    }
}
