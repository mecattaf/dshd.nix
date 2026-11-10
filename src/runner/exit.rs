//! The runner's exit status table: one pure function over the transport,
//! readiness, whether a stop was requested, and how the child ended.

use crate::record::{ChildEnd, Transport};
use crate::runner::EXIT_TEMPFAIL;
use crate::sys::SIGTERM;

/// What row (d) passes instead of 75, which would ask systemd for a
/// restart the table did not grant.
const EXIT_FAILURE: u8 = 1;

/// The table:
/// (a) child exited 0 → 0;
/// (b) stop requested and child exited 0 or died by SIGTERM → 0;
/// (c) web or acp, ready, no stop requested, child non-zero or signalled → 75;
/// (d) otherwise pass the child's code through: 75 becomes 1, signal S
/// becomes 128+S.
#[must_use]
pub fn exit_status(
    transport: Transport,
    became_ready: bool,
    stop_requested: bool,
    child: ChildEnd,
) -> u8 {
    let exited_clean = child.exit_code == Some(0);
    let terminated = child.signal == Some(SIGTERM);
    let restartable = matches!(transport, Transport::Web | Transport::Acp);
    if exited_clean || (stop_requested && terminated) {
        0
    } else if restartable && became_ready && !stop_requested {
        EXIT_TEMPFAIL
    } else {
        passthrough(child)
    }
}

/// Row (d): the status a shell would report for the child's end, except
/// that 75 becomes 1.
fn passthrough(child: ChildEnd) -> u8 {
    match child.shell_status() {
        EXIT_TEMPFAIL => EXIT_FAILURE,
        status => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGKILL: i32 = 9;

    fn exited(code: i32) -> ChildEnd {
        ChildEnd {
            exit_code: Some(code),
            signal: None,
        }
    }

    fn killed(signal: i32) -> ChildEnd {
        ChildEnd {
            exit_code: None,
            signal: Some(signal),
        }
    }

    // catches: a row of the exit table that restarts a stopped or unready instance, misses a crash after
    // readiness, or passes 75 through as a restart request (P1.7)
    #[test]
    fn table() {
        use Transport::{Acp, None as Headless, Web};
        let rows = [
            // (a) a clean exit is 0 whatever else happened.
            (Web, true, false, exited(0), 0),
            (Acp, false, true, exited(0), 0),
            (Headless, true, false, exited(0), 0),
            // (b) a requested stop that ended by SIGTERM is 0.
            (Web, true, true, killed(SIGTERM), 0),
            (Headless, true, true, killed(SIGTERM), 0),
            (Acp, false, true, killed(SIGTERM), 0),
            // (c) web or acp, ready, no stop: any other end asks for a restart.
            (Web, true, false, exited(1), 75),
            (Web, true, false, exited(42), 75),
            (Web, true, false, exited(75), 75),
            (Acp, true, false, killed(SIGKILL), 75),
            (Acp, true, false, killed(SIGTERM), 75),
            // (d) pass through: before readiness, headless, or a stop that ended otherwise.
            (Web, false, false, exited(1), 1),
            (Web, false, false, exited(75), 1),
            (Acp, false, false, killed(SIGKILL), 137),
            (Headless, true, false, exited(3), 3),
            (Headless, true, false, exited(75), 1),
            (Headless, true, false, killed(SIGKILL), 137),
            (Headless, true, false, killed(SIGTERM), 143),
            (Web, true, true, exited(1), 1),
            (Web, true, true, killed(SIGKILL), 137),
            (Headless, false, false, killed(200), 255),
            (
                Headless,
                false,
                false,
                ChildEnd {
                    exit_code: None,
                    signal: None,
                },
                1,
            ),
        ];
        for (index, (transport, ready, stop, child, expected)) in rows.into_iter().enumerate() {
            assert_eq!(
                exit_status(transport, ready, stop, child),
                expected,
                "row {index}: {transport:?} {child:?}"
            );
        }
    }
}
