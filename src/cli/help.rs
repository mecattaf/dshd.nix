//! `dshd help [VERB]`: the general page names every verb and every exit
//! code; a verb page shows its flags. Plain text on stdout, exit 0.

use super::parse::Word;
use crate::error::Error;
use crate::out;

/// The general page.
const GENERAL: &str = r#"dshd: durable native DSH sessions under the systemd user manager

Usage: dshd VERB [FLAGS] [NAME] [-- DSH ARGUMENTS]

Verbs:
  run       create an instance from -- DSH ARGUMENTS and start it; a headless task runs in the foreground
  start     start an instance again from its recorded recipe
  stop      stop an instance; its recipe and release stay
  restart   restart an instance on its current release
  update    point an instance at what its release name resolves to now
  ls        every instance and its state; dshd alone does the same
  get       everything dshd knows about one instance
  read      print the journal of an instance's current or last run
  wait      wait until an instance's run ends, and exit as dsh did
  attach    print or open the web URL, or attach to the ACP relay; never starts
  label     set or remove the labels of an instance
  enable    start an instance with the user manager
  disable   stop starting an instance with the user manager
  rm        delete a stopped instance and its state
  help      this page, or the page of one verb: dshd help VERB

Flags come before --; everything after it goes to dsh unchanged.
Results print as JSON when stdout is not a terminal or with --json; --text
forces the human form. read, attach, help and a headless run in the
foreground print text unless --json is given. Errors print as JSON by the
first rule, as {"error":{"kind":...,"message":...,"hint":...}} on stderr.
dshd VERB --help shows the flags of a verb; dshd --version prints the version.

Exit codes:
  0    success
  1    operation failed: start-failed stop-failed release-invalid io inconsistent-state systemctl-failed
  2    usage: invalid-argument invalid-name needs-target
  3    not found: not-found release-not-found
  4    conflict: exists-different-recipe instance-running task-already-ran unit-masked attached-elsewhere
  5    not running: not-running
  6    unsupported: unsupported-transport unsupported-profile autostart-not-allowed
  7    environment unavailable: systemd-unavailable tool-missing template-missing
  124  still running at check-in (run, wait); the instance continues
  N    run and wait in text mode: dsh's own exit code, or 128+S when signal S ended it
Exit codes are stable once announced.

Agent? Run: dshd --skill"#;

/// What a stop, restart or update from inside the instance does.
const FROM_INSIDE: &str =
    "Run from inside the instance, the command ends with the instance, and systemd
still completes the action; dshd get NAME from outside shows the result.";

const RUN: &str = r"Usage: dshd run [--bg] [--name NAME] [--release RELEASE] [--cwd DIR] [--label KEY=VALUE]... [--patch FILE]... [--timeout SECONDS] [--json | --text] -- DSH ARGUMENTS

Creates an instance that runs `dsh DSH ARGUMENTS` in DIR with this shell's
environment, as the unit dshd@NAME, and starts it. A NAME that exists with
the same recipe is started and its labels are set; one with a different
recipe is refused. The recipe is the directory, the arguments, the profile,
the transport, the content of each --patch FILE, and the release when
--release is given. An instance that runs already is reported, not
restarted.

--patch FILE copies FILE into the instance at creation; every start hands
dsh that copy with --patch right after the profile, before the --patch
flags in DSH ARGUMENTS, and FILE is never read again.

dsh runs the profile as it is in its DSH home at every start. dshd only
reads the profile's bundles, which fix the transport at creation; a shipped
profile that does not exist yet is created by dsh itself.

A web or acp instance, or any instance with --bg, is left running and run
returns. A headless instance runs in the foreground: its output is printed
until dsh exits, and dshd exits as dsh did, also when stdout is not a
terminal. At --timeout run exits 124 and the instance continues; dshd wait
NAME picks it up. With --json, run prints how the run ended and exits 0;
dshd read NAME has the output. run never runs a headless task twice: dshd
start --rerun NAME does that on purpose.

  --bg                return once the instance runs, also for a headless task
  --name NAME         instance name; default PROFILE-K with the lowest free K
  --release RELEASE   a release under /etc/dshd/releases, or a release store path;
                      default: default
  --cwd DIR           working directory; default: the current directory
  --label KEY=VALUE   metadata shown by get and ls; KEY matches [a-z0-9._-]+
  --patch FILE        an overlay to snapshot, a regular file of at most 1 MiB;
                      repeatable, applied in order
  --timeout SECONDS   how long a foreground run follows its task; default 180
  --json, --text      the output format";

const START: &str = r"Usage: dshd start [--rerun] [--json | --text] NAME

Starts NAME again from its recorded recipe and its current release, and
waits until it runs. An instance that runs already is reported, not
restarted. dshd run creates instances; start never does. A start fails
when the profile now selects another transport than the recorded one.

A headless instance whose task already ran is refused, since starting it
submits the recorded prompt again; --rerun does that on purpose.

  --rerun          run a headless instance's task again
  --json, --text   the output format";

const STOP: &str = r"Usage: dshd stop [--invocation ID] [--json | --text] NAME

Stops dshd@NAME and every process it started, tool children included. The
recipe, the release roots and DSH's own sessions stay; dshd start NAME
starts the instance again.

--invocation ID stops only the run of that systemd invocation, the id
dshd get NAME shows as invocationId or lastRun.invocationId: the unit is
stopped while it runs ID, and already-stopped when it is stopped and its
last run was ID. A unit on another invocation, running or last, and an
instance that never ran are refused with not-running, and nothing is
stopped. dshd's own starts of NAME wait until the comparison and the stop
are done; a restart systemd makes after a crash, or a start from outside
dshd, is not fenced.

  --invocation ID  the 32 hex digits of the run to stop
  --json, --text   the output format";

const RESTART: &str = r"Usage: dshd restart [--json | --text] NAME

Clears a failed state, restarts dshd@NAME on the instance's current
release and waits until it runs. A headless instance is refused, since a
restart would submit its task again; dshd start --rerun NAME does that on
purpose.";

const UPDATE: &str = r"Usage: dshd update [-R] [--json | --text] NAME

Points the instance's current release at what its release name under
/etc/dshd/releases resolves to now, and prints what nix store diff-closures
reports between the two releases. A running instance keeps its booted release until it restarts. An instance
created from a release store path is pinned to it: remove it, then run it
again with --release RELEASE.

  -R, --restart    restart the instance when it runs and its booted release is
                   not its current one; a headless instance is not restarted,
                   and the update applies at its next dshd start --rerun NAME
  --json, --text   the output format";

const LS: &str = r#"Usage: dshd ls [--json | --text]

Every instance: NAME: state(release), followed by the command to run when
one is needed. dshd alone does the same. As JSON, one object:
{"instances":[...],"linger":...,"warnings":[...]}."#;

const GET: &str = r"Usage: dshd get [--json | --text] NAME

The unit, the recorded recipe and profile, the release links, the web
address or the ACP relay, how the last run ended, and whether linger is on.";

const READ: &str = r#"Usage: dshd read [-n LINES] [-f] [--json] NAME

Prints the journal of the instance's current run, or of its last one, as
dsh wrote it. When the lines do not reach back to the start of the run, a
note on stderr says so. The journal of an acp instance holds dsh's stderr
only: its stdout is the ACP channel.

  -n, --lines LINES   lines to show; default 200
  -f, --follow        keep printing new lines until the run ends
  --json              {"name","invocationId","complete","streams","lines"}"#;

const WAIT: &str = r"Usage: dshd wait [--timeout SECONDS] [--json | --text] NAME

Waits until the instance's current run ends, then exits as dsh did: with its
own exit code, or 128+S when signal S ended it. A run that ended already is
reported at once. At --timeout wait exits 124 and the instance continues.
As JSON it exits 0, with the outcome ended or still-running and the fields
exitCode, signal and reason.

  --timeout SECONDS   how long to wait; default 180
  --json, --text      the output format";

const ATTACH: &str = r#"Usage: dshd attach [--via web|acp] [--open | --print-url] [--takeover] [--json] NAME

Connects to a running instance through its transport; it never starts one.
web: prints the origin of the instance's web address. A browser that holds
its cookie opens it as is; --print-url prints the full authenticated URL,
the only output that holds the token.
acp: runs the ACP relay client on this terminal's stdin and stdout. The
relay has one controller; --takeover replaces the one attached.
A headless instance has nothing to attach to; dshd read --follow NAME shows
its output.

  --via web|acp   the transport; default: the instance's own
  --open          hand the full URL to xdg-open
  --print-url     print the full authenticated URL
  --takeover      replace the ACP controller that is attached
  --json          web only: {"origin","port","urlFile"}, and "url" with --print-url"#;

const LABEL: &str = r"Usage: dshd label [--json | --text] NAME KEY=VALUE...

Sets labels of an instance; an empty VALUE removes KEY. KEY matches
[a-z0-9._-]+. Labels are metadata for get and ls; dsh never sees them, and
the instance keeps running.";

const ENABLE: &str = r"Usage: dshd enable [--json | --text] NAME

Adds dshd@NAME to dshd.target, which the user manager starts at the
first login, or at boot when linger is on: links it into
dshd.target.wants of the manager's unit directory and reloads the
manager. Starts nothing. A headless
instance cannot be enabled: every start runs its task again.";

const DISABLE: &str = r"Usage: dshd disable [--json | --text] NAME

Removes dshd@NAME from dshd.target. Stops nothing.";

const RM: &str = r"Usage: dshd rm [--json | --text] NAME

Deletes a stopped instance: its state directory and its release roots,
which the next garbage collection may then free. The profile and DSH's own
sessions stay.";

const HELP: &str = r"Usage: dshd help [VERB]

Prints the general page, or the page of VERB. dshd VERB --help does the
same, and dshd --skill prints the guide for agents.";

/// Prints the general help, or the page for `topic`.
pub(super) fn run(topic: Option<&str>) -> Result<(), Error> {
    match topic {
        None => out::line(GENERAL),
        Some(verb) => match Word::parse(verb) {
            Some(word) => out::line(&page(word)),
            None => Err(Error::InvalidArgument(format!(
                "no help for {verb}: it is not a verb"
            ))),
        },
    }
}

/// The page of one verb. Those that can act from inside the instance end
/// with what that means.
fn page(word: Word) -> String {
    let text = match word {
        Word::Run => RUN,
        Word::Start => START,
        Word::Stop => STOP,
        Word::Restart => RESTART,
        Word::Update => UPDATE,
        Word::Ls => LS,
        Word::Get => GET,
        Word::Read => READ,
        Word::Wait => WAIT,
        Word::Attach => ATTACH,
        Word::Label => LABEL,
        Word::Enable => ENABLE,
        Word::Disable => DISABLE,
        Word::Rm => RM,
        Word::Help => HELP,
    };
    match word {
        Word::Stop | Word::Restart | Word::Update => format!("{text}\n\n{FROM_INSIDE}"),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::error::{OtherInvocation, RecipeDifference, StartFailure, ToolFailure, UnheldApp};

    /// The agent guide, which names the same verbs.
    const SKILL: &str = include_str!("skill.md");

    fn tool(tool: &'static str) -> Error {
        Error::Tool(Box::new(ToolFailure {
            tool,
            args: vec![],
            status: Some(1),
            stderr: String::new(),
        }))
    }

    /// One value of every variant, so a new variant without a row fails here.
    fn every_error() -> Vec<Error> {
        let name = || "x".to_owned();
        vec![
            Error::InvalidName {
                name: name(),
                reason: "r",
            },
            Error::InvalidArgument(name()),
            Error::NeedsTarget {
                verb: "stop",
                candidates: None,
            },
            Error::CreateWithRun(name()),
            Error::PathPinned(name()),
            Error::NotFound(name()),
            Error::ReleaseNotFound(name()),
            Error::ReleaseInvalid {
                release: name(),
                reason: name(),
            },
            Error::ExistsDifferentRecipe(Box::new(RecipeDifference {
                name: name(),
                fields: vec![],
            })),
            Error::InstanceRunning(name()),
            Error::TaskAlreadyRan(name()),
            Error::UnitMasked(name()),
            Error::NotRunning(name()),
            Error::OtherInvocation(Box::new(OtherInvocation {
                name: name(),
                expected: name(),
                found: None,
                up: false,
            })),
            Error::AttachedElsewhere(name()),
            Error::UnsupportedTransport {
                name: name(),
                transport: "none",
            },
            Error::UnsupportedTransport {
                name: name(),
                transport: "web",
            },
            Error::UnsupportedProfile {
                profile: name(),
                app: UnheldApp::Sdk,
            },
            Error::UnsupportedProfile {
                profile: name(),
                app: UnheldApp::Other,
            },
            Error::AutostartNotAllowed(name()),
            Error::InconsistentState {
                name: name(),
                reason: name(),
            },
            Error::SystemdUnavailable(name()),
            Error::ForeignSession(name()),
            Error::ToolMissing("journalctl"),
            Error::TemplateMissing,
            tool("systemctl"),
            tool("nix-store"),
            tool("journalctl"),
            Error::StartFailed(Box::new(StartFailure {
                name: name(),
                active_state: name(),
                result: name(),
                journal_tail: vec![],
            })),
            Error::StopFailed {
                name: name(),
                detail: name(),
            },
            Error::io("read", "/srv/x", io::Error::from(io::ErrorKind::Other)),
            Error::Output(io::Error::from(io::ErrorKind::BrokenPipe)),
        ]
    }

    // catches: a verb or an error kind added without its line on the general page or in the agent guide
    #[test]
    fn names_every_verb_and_exit() {
        for word in Word::ALL {
            let verb = word.text();
            assert!(
                GENERAL.contains(&format!("\n  {verb} ")),
                "general page lacks {verb}"
            );
            assert!(
                page(word).starts_with(&format!("Usage: dshd {verb}")),
                "no page for {verb}"
            );
            assert!(
                SKILL.contains(&format!("dshd {verb}")),
                "the skill never shows dshd {verb}"
            );
        }
        for code in [0, 1, 2, 3, 4, 5, 6, 7, 124] {
            assert!(
                GENERAL.contains(&format!("\n  {code}  ")),
                "general page lacks exit {code}"
            );
        }
        assert!(
            GENERAL.contains("\n  N    run and wait in text mode"),
            "general page lacks the mirrored exit"
        );
        for error in every_error() {
            if error.exit_code() > 0 {
                let row = GENERAL
                    .lines()
                    .find(|l| l.starts_with(&format!("  {}  ", error.exit_code())))
                    .unwrap();
                assert!(
                    row.split(' ').any(|word| word == error.kind()),
                    "exit {} row lacks {}",
                    error.exit_code(),
                    error.kind()
                );
            }
        }
        assert!(GENERAL.ends_with("\nAgent? Run: dshd --skill"));
        assert!(
            SKILL.lines().count() < 500,
            "the skill is loaded whole into an agent's context"
        );
    }

    // catches: a flag the parser takes that its help page does not name
    #[test]
    fn pages_name_their_flags() {
        for word in Word::ALL {
            let text = page(word);
            for flag in word.flags() {
                assert!(text.contains(flag), "{} page lacks {flag}", word.text());
            }
        }
        assert!(!page(Word::Read).contains("--text"));
        assert!(!page(Word::Attach).contains("--text"));
        for word in [Word::Stop, Word::Restart, Word::Update] {
            assert!(page(word).ends_with(FROM_INSIDE), "{}", word.text());
        }
    }

    // catches: a remedy that names no command, or a command dshd does not have
    #[test]
    fn hints_name_a_command() {
        let pointers = [
            "invalid-name",
            "release-not-found",
            "tool-missing",
            "template-missing",
        ];
        for error in every_error() {
            let Some(hint) = error.hint() else {
                continue;
            };
            let pointer = pointers.contains(&error.kind())
                || matches!(
                    error,
                    Error::UnsupportedProfile { .. }
                        | Error::SystemdUnavailable(_)
                        | Error::ForeignSession(_)
                );
            if pointer {
                assert!(!hint.is_empty(), "{error:?}");
                continue;
            }
            let words: Vec<&str> = hint.split(' ').collect();
            match words.as_slice() {
                ["dshd", verb, ..] => assert!(Word::parse(verb).is_some(), "{hint}"),
                ["systemctl", "--user", ..] => {}
                _ => panic!("{error:?}: the hint {hint:?} names no command"),
            }
        }
    }
}
