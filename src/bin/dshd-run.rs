//! `dshd-run`: the per-release runner and the only ACP relay client.
//!
//! Invoked by the unit template as `dshd-run run NAME` (`ExecStart`) and
//! `dshd-run poststop NAME` (`ExecStopPost`), and by the CLI as
//! `dshd-run attach NAME [--takeover]` and `dshd-run probe NAME` through the
//! instance's `booted` link, so the client always matches the relay it talks
//! to. This is the only place the runner binary calls `process::exit`.

use std::ffi::OsString;
use std::process;

use dshd::relay;
use dshd::runner;

/// Exit status for a malformed invocation of the runner itself. The template
/// never produces one, so it is reported, never retried (`Restart=no`).
const EXIT_USAGE: u8 = 64;

fn dispatch(args: &[OsString]) -> u8 {
    let verb = args.first().and_then(|a| a.to_str());
    let name = args.get(1).and_then(|a| a.to_str());
    let takeover_flag = args.get(2).and_then(|a| a.to_str());
    match (verb, name, args.len()) {
        (Some("run"), Some(name), 2) => runner::run(name),
        (Some("poststop"), Some(name), 2) => runner::poststop(name),
        (Some("attach"), Some(name), 2) => relay::attach(name, false),
        (Some("attach"), Some(name), 3) if takeover_flag == Some("--takeover") => {
            relay::attach(name, true)
        }
        (Some("probe"), Some(name), 2) => relay::probe(name),
        _ => {
            runner::log_line("usage: dshd-run run|poststop|attach [--takeover]|probe NAME");
            EXIT_USAGE
        }
    }
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    process::exit(i32::from(dispatch(&args)));
}
