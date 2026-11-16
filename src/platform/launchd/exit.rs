//! What a failed `launchctl` call means. launchctl reports errno-style
//! codes as its exit status; dshd reads only the few that change what it
//! does. The table follows the codes macOS 14 and 15 print (`launchctl
//! error <code>` explains each); the real-Mac test session confirms them.

use crate::error::Error;

/// `No such process`: `bootout` of a job launchd does not hold.
pub const NO_SUCH_PROCESS: i32 = 3;
/// `Input/output error`: `bootstrap` of a label launchd already holds, or
/// a plist it refused to load.
pub const IO_ERROR: i32 = 5;
/// `Could not find specified domain`: no `gui/<uid>` domain, because the
/// user is not logged in at the Mac.
pub const NO_DOMAIN: i32 = 112;
/// `Could not find specified service`: `print` of a job launchd does not
/// hold.
pub const NO_SERVICE: i32 = 113;
/// `Domain does not support specified action`: `bootstrap` into a GUI
/// domain from a session that cannot reach it.
pub const DOMAIN_REFUSES: i32 = 125;

/// The hint every missing-domain error carries.
pub const LOGIN_HINT: &str = "LaunchAgents run only in a login session: log in to the Mac as this user (at the screen, or by Screen Sharing), then run dshd again; over SSH dshd works while that login lasts";

/// A classified `launchctl` failure.
#[derive(Debug)]
pub enum Failure {
    /// launchd holds no such job (`print` or `bootout` of a missing label).
    NoSuchJob,
    /// Anything else, as the error to report. A missing GUI domain is
    /// `SystemdUnavailable` (exit 7) with [`LOGIN_HINT`].
    Other(Error),
}

/// Classifies the error of one `launchctl` call.
#[must_use]
pub fn classify(error: Error) -> Failure {
    let Error::Tool(failure) = error else {
        return Failure::Other(error);
    };
    if failure.tool != "launchctl" {
        return Failure::Other(Error::Tool(failure));
    }
    match failure.status {
        Some(NO_SERVICE | NO_SUCH_PROCESS) => Failure::NoSuchJob,
        Some(NO_DOMAIN | DOMAIN_REFUSES) => Failure::Other(Error::SystemdUnavailable(format!(
            "launchctl {} found no GUI login session for this user ({}); {LOGIN_HINT}",
            failure.args.join(" "),
            failure.stderr
        ))),
        _ => Failure::Other(Error::Tool(failure)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolFailure;

    fn failed(tool: &'static str, status: i32) -> Error {
        Error::Tool(Box::new(ToolFailure {
            tool,
            args: vec!["print".to_owned(), "gui/501/dshd.x".to_owned()],
            status: Some(status),
            stderr: "launchctl said no".to_owned(),
        }))
    }

    // catches: a missing job reported as a failure, or a missing GUI session reported as a generic tool failure
    #[test]
    fn classifies_the_codes_dshd_acts_on() {
        assert!(matches!(
            classify(failed("launchctl", NO_SERVICE)),
            Failure::NoSuchJob
        ));
        assert!(matches!(
            classify(failed("launchctl", NO_SUCH_PROCESS)),
            Failure::NoSuchJob
        ));
        for code in [NO_DOMAIN, DOMAIN_REFUSES] {
            match classify(failed("launchctl", code)) {
                Failure::Other(error) => {
                    assert_eq!(error.exit_code(), 7, "{code}");
                    assert!(error.to_string().contains("login session"), "{error}");
                }
                Failure::NoSuchJob => panic!("{code}"),
            }
        }
        match classify(failed("launchctl", IO_ERROR)) {
            Failure::Other(Error::Tool(failure)) => assert_eq!(failure.status, Some(IO_ERROR)),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify(failed("systemctl", NO_SERVICE)),
            Failure::Other(Error::Tool(_))
        ));
        assert!(matches!(
            classify(Error::ToolMissing("launchctl")),
            Failure::Other(Error::ToolMissing("launchctl"))
        ));
    }
}
