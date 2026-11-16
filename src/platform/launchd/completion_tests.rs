//! The macOS end words.

use super::*;

fn end(result: &str, code: &str, status: &str) -> EndWords {
    words(result, code, status)
}

// catches: a runner's own end recorded in words the Linux records and the state mapping do not use
#[test]
fn own_ends() {
    assert_eq!(own_end(0, false), end("success", "exited", "0"));
    assert_eq!(own_end(75, false), end("exit-code", "exited", "75"));
    assert_eq!(own_end(1, false), end("exit-code", "exited", "1"));
    assert_eq!(own_end(1, true), end("timeout", "killed", "TERM"));
}

// catches: a runner killed by launchd or a crash reconciled as success, or a never-exited job given an end
#[test]
fn reconciled_ends() {
    assert_eq!(
        reconciled_end(&LastExit::Signal(9)),
        Some(end("signal", "killed", "KILL"))
    );
    assert_eq!(
        reconciled_end(&LastExit::Code(0)),
        Some(end("exit-code", "exited", "0"))
    );
    assert_eq!(
        reconciled_end(&LastExit::Code(78)),
        Some(end("exit-code", "exited", "78"))
    );
    assert_eq!(reconciled_end(&LastExit::Never), None);
}

// catches: signal names taken from another platform's numbering
#[test]
fn signal_words_follow_this_platform() {
    assert_eq!(signal_word(libc::SIGTERM), "TERM");
    assert_eq!(signal_word(libc::SIGKILL), "KILL");
    assert_eq!(signal_word(libc::SIGSEGV), "SEGV");
    assert_eq!(signal_word(libc::SIGBUS), "BUS");
    assert_eq!(signal_word(libc::SIGUSR1), "USR1");
    assert_eq!(signal_word(200), "200");
    assert_eq!(start_limit_end(), end("start-limit-hit", "exited", "0"));
}
