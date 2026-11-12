//! The CLI contract: the verbs and their flags as parsed values, the
//! context every verb runs with, the output format a result is printed in,
//! and `main`, which maps parse, run and error to one exit code.
//!
//! Grammar: `dshd VERB [FLAGS] [NAME] [-- DSH ARGUMENTS]`. Flags come
//! before `--`, and only `run` takes arguments after it. Results print as
//! JSON when stdout is not a terminal or with `--json`, and as text on a
//! terminal or with `--text`; `read`, `attach` and `help` always print
//! text. The parser and the verb bodies live in the submodules, the JSON
//! objects in `report`; this file is the shape they agree on.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::Error;
use crate::instance;
use crate::name::{InstanceName, StateRoot};
use crate::out;
use crate::record::Transport;
use crate::systemctl::Systemctl;
use crate::tool::Tool;

mod attach;
mod context;
mod follow;
mod help;
mod journal;
mod label;
mod lifecycle;
mod ls;
mod parse;
mod read;
mod render;
/// The JSON objects the CLI prints, which `tests/contract/dshd.schema.json` describes.
pub mod report;
mod rm;
mod run;
mod start;
mod status;
mod update;
mod wait;

/// Default number of journal lines `read` prints.
pub const READ_DEFAULT_LINES: u32 = 200;
/// Default seconds a foreground `run` or a `wait` follows before it
/// checks in with exit 124.
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;

/// The agent guide `dshd --skill` prints.
const SKILL: &str = include_str!("cli/skill.md");

/// `dshd run [--bg] [--name N] [--release R] [--cwd D] [--label K=V]...
/// [--patch PATH]... [--timeout S] -- DSH ARGUMENTS`: create the instance,
/// or match the stored recipe of `--name`, and start it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// `--name N`; default `PROFILE-K` with the lowest free K.
    pub name: Option<InstanceName>,
    /// `--release R`, a registry name or a release store path.
    pub release: Option<String>,
    /// `--cwd D`. Defaults to the client's physical working directory.
    pub cwd: Option<PathBuf>,
    /// `--label KEY=VALUE`, repeatable. An empty value removes the key.
    pub labels: BTreeMap<String, String>,
    /// `--patch PATH`, repeatable: overlays dshd snapshots into the instance at creation.
    pub patches: Vec<PathBuf>,
    /// `--bg`: return once the instance runs, also for a headless task.
    pub bg: bool,
    /// `--timeout S`: how long a foreground headless run follows its task.
    pub timeout: u64,
    /// Everything after `--`, verbatim, never empty.
    pub argv: Vec<OsString>,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            name: None,
            release: None,
            cwd: None,
            labels: BTreeMap::new(),
            patches: Vec::new(),
            bg: false,
            timeout: DEFAULT_TIMEOUT_SECS,
            argv: Vec::new(),
        }
    }
}

/// `dshd start [--rerun] NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartArgs {
    /// The instance.
    pub name: InstanceName,
    /// `--rerun`: start a headless instance whose task already ran.
    pub rerun: bool,
}

/// A verb that takes one NAME.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameArgs {
    /// The instance.
    pub name: InstanceName,
}

/// `dshd stop [--invocation ID] NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopArgs {
    /// The instance.
    pub name: InstanceName,
    /// `--invocation ID`: stop only the run of this systemd invocation id,
    /// 32 lowercase hex digits.
    pub invocation: Option<String>,
}

/// `dshd update [-R] NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateArgs {
    /// The instance.
    pub name: InstanceName,
    /// `-R`: restart after a successful update when the instance runs.
    pub restart: bool,
}

/// `dshd ls`, or `dshd` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LsArgs {
    /// No verb was given: on a terminal the list ends with where to look next.
    pub bare: bool,
}

/// `dshd read NAME [-n LINES | --head LINES] [-f]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadArgs {
    /// The instance.
    pub name: InstanceName,
    /// `-n LINES` or `--head LINES`: lines to show (default
    /// [`READ_DEFAULT_LINES`]).
    pub lines: u32,
    /// `--head`: the run's oldest `lines` lines, from its start, instead of
    /// its newest.
    pub head: bool,
    /// `-f`: keep printing new lines until the run ends.
    pub follow: bool,
}

/// `dshd wait NAME [--timeout S]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitArgs {
    /// The instance.
    pub name: InstanceName,
    /// Seconds to follow before checking in with exit 124.
    pub timeout: u64,
}

/// `dshd attach NAME [--via web|acp] [--takeover] [--print-url] [--open]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachArgs {
    /// The instance.
    pub name: InstanceName,
    /// `--via`: defaults to the instance's recorded transport.
    pub via: Option<Transport>,
    /// `--open`: hand the web URL to `xdg-open` instead of printing it.
    pub open: bool,
    /// `--print-url`: print the full authenticated web URL.
    pub print_url: bool,
    /// `--takeover`: replace the current ACP controller.
    pub takeover: bool,
}

/// `dshd label NAME KEY=VALUE...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelArgs {
    /// The instance.
    pub name: InstanceName,
    /// The labels to set; an empty value removes the key.
    pub labels: BTreeMap<String, String>,
}

/// One parsed verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// `run ... -- DSH ARGUMENTS`.
    Run(RunArgs),
    /// `start [--rerun] NAME`.
    Start(StartArgs),
    /// `stop [--invocation ID] NAME`.
    Stop(StopArgs),
    /// `restart NAME`.
    Restart(NameArgs),
    /// `update [-R] NAME`.
    Update(UpdateArgs),
    /// `ls`, and `dshd` alone.
    Ls(LsArgs),
    /// `get NAME`.
    Get(NameArgs),
    /// `read NAME [-n LINES | --head LINES] [-f]`.
    Read(ReadArgs),
    /// `wait NAME [--timeout S]`.
    Wait(WaitArgs),
    /// `attach NAME ...`.
    Attach(AttachArgs),
    /// `label NAME KEY=VALUE...`.
    Label(LabelArgs),
    /// `enable NAME`.
    Enable(NameArgs),
    /// `disable NAME`.
    Disable(NameArgs),
    /// `rm NAME`.
    Rm(NameArgs),
    /// `help [VERB]`, and `--help` anywhere.
    Help(Option<String>),
    /// `--skill`.
    Skill,
    /// `--version`.
    Version,
}

/// The output format asked for on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Neither flag: JSON when stdout is not a terminal.
    Default,
    /// `--json`.
    Json,
    /// `--text`.
    Text,
}

/// Where results and errors print as JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Formats {
    /// The result on stdout is JSON.
    results_json: bool,
    /// An error on stderr is JSON.
    errors_json: bool,
}

/// What every verb body gets. Built once per invocation from the process
/// environment and the user manager.
#[derive(Debug)]
pub struct Context {
    /// The format flag given.
    pub asked: Asked,
    /// Results print as JSON.
    pub json: bool,
    /// The user manager.
    pub systemctl: Systemctl,
    /// `%S`: the manager's state root.
    pub state_root: StateRoot,
    /// `%t`: the manager's runtime root (`XDG_RUNTIME_DIR`).
    pub runtime_root: PathBuf,
    /// `nix-store`, for the release GC roots.
    pub nix_store: Tool,
    /// `nix`, for the closure difference `update` prints.
    pub nix: Tool,
    /// `journalctl`, for `read`, `wait` and failure tails.
    pub journalctl: Tool,
    /// `xdg-open`, for `attach --open`.
    pub xdg_open: Tool,
    /// The invoking user's home, from `getpwuid`, never `$HOME` alone.
    pub home: PathBuf,
    /// The invoking user's name.
    pub user: String,
    /// The client's environment, captured at process start.
    pub environment: Vec<(OsString, OsString)>,
    /// The client's physical working directory.
    pub cwd: PathBuf,
}

impl Context {
    /// Builds the context: user from the passwd database, state and runtime
    /// roots from the user manager, tools from `PATH`, environment and cwd
    /// from the process. A manager that does not answer is
    /// `SystemdUnavailable`.
    pub fn from_env(asked: Asked, json: bool) -> Result<Self, Error> {
        context::from_env(asked, json)
    }
}

/// The format flag among `args` (without `argv[0]`), before `--` only.
/// `--json` with `--text` is `InvalidArgument`.
fn asked(args: &[OsString]) -> Result<Asked, Error> {
    let head = args.iter().take_while(|arg| *arg != "--");
    let (mut json, mut text) = (false, false);
    for arg in head {
        json |= arg == "--json";
        text |= arg == "--text";
    }
    match (json, text) {
        (true, true) => Err(Error::InvalidArgument(
            "--json and --text exclude each other".into(),
        )),
        (true, false) => Ok(Asked::Json),
        (false, true) => Ok(Asked::Text),
        (false, false) => Ok(Asked::Default),
    }
}

/// Parses the process arguments (without `argv[0]`) into a verb.
pub fn parse(args: Vec<OsString>) -> Result<Verb, Error> {
    parse::parse(args)
}

/// Parses, runs and maps to the exit code. The only function `main.rs` calls.
pub fn main(args: Vec<OsString>) -> u8 {
    let piped = !out::stdout_is_terminal();
    let asked = match asked(&args) {
        Ok(asked) => asked,
        Err(error) => return out::fail(piped, &error),
    };
    let verb = match parse(args) {
        Ok(verb) => verb,
        Err(error) => {
            let errors_json = asked == Asked::Json || (asked == Asked::Default && piped);
            return out::fail(errors_json, &with_candidates(error, asked));
        }
    };
    let formats = formats(asked, piped, &verb);
    let result = match verb {
        Verb::Help(topic) => help::run(topic.as_deref()).map(|()| 0),
        Verb::Skill => out::write(SKILL.as_bytes()).map(|()| 0),
        Verb::Version => out::line(&format!("dshd {}", env!("CARGO_PKG_VERSION"))).map(|()| 0),
        verb => Context::from_env(asked, formats.results_json).and_then(|ctx| run(&ctx, verb)),
    };
    result.unwrap_or_else(|error| out::fail(formats.errors_json, &error))
}

/// Where results and errors print as JSON: with `--json`, or by default
/// when stdout is not a terminal. `read`, `attach`, `help`, `--skill` and
/// `--version` print text results unless `--json` was given.
fn formats(asked: Asked, piped: bool, verb: &Verb) -> Formats {
    let text_verb = matches!(
        verb,
        Verb::Read(_) | Verb::Attach(_) | Verb::Help(_) | Verb::Skill | Verb::Version
    );
    let by_default = asked == Asked::Default && piped;
    Formats {
        results_json: asked == Asked::Json || (by_default && !text_verb),
        errors_json: asked == Asked::Json || by_default,
    }
}

/// A `NeedsTarget` error with the instances that exist, when they can be
/// listed; any other error unchanged.
fn with_candidates(error: Error, asked: Asked) -> Error {
    let Error::NeedsTarget {
        verb,
        candidates: None,
    } = error
    else {
        return error;
    };
    let listed =
        Context::from_env(asked, false).and_then(|ctx| instance::discover(&ctx.state_root));
    let candidates = listed
        .ok()
        .map(|found| found.names.iter().map(ToString::to_string).collect());
    Error::NeedsTarget { verb, candidates }
}

fn run(ctx: &Context, verb: Verb) -> Result<u8, Error> {
    let done = |result: Result<(), Error>| result.map(|()| 0);
    match verb {
        Verb::Run(args) => run::run(ctx, &args),
        Verb::Wait(args) => wait::run(ctx, &args),
        Verb::Start(args) => done(start::run(ctx, &args)),
        Verb::Stop(args) => done(lifecycle::stop(ctx, &args)),
        Verb::Restart(args) => done(lifecycle::restart(ctx, &args)),
        Verb::Enable(args) => done(lifecycle::enable(ctx, &args)),
        Verb::Disable(args) => done(lifecycle::disable(ctx, &args)),
        Verb::Update(args) => done(update::run(ctx, &args)),
        Verb::Ls(args) => done(ls::run(ctx, args)),
        Verb::Get(args) => done(status::run(ctx, &args)),
        Verb::Read(args) => done(read::run(ctx, &args)),
        Verb::Attach(args) => done(attach::run(ctx, &args)),
        Verb::Label(args) => done(label::run(ctx, &args)),
        Verb::Rm(args) => done(rm::run(ctx, &args)),
        Verb::Help(_) | Verb::Skill | Verb::Version => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    // catches: piped output printed as text, a text verb printed as JSON, or --text ignored for errors
    #[test]
    fn format_table() {
        let name = InstanceName::parse("a").unwrap();
        let get = Verb::Get(NameArgs { name: name.clone() });
        let read = Verb::Read(ReadArgs {
            name,
            lines: READ_DEFAULT_LINES,
            head: false,
            follow: false,
        });
        let rows = [
            (Asked::Default, true, &get, true, true),
            (Asked::Default, false, &get, false, false),
            (Asked::Json, false, &get, true, true),
            (Asked::Text, true, &get, false, false),
            (Asked::Default, true, &read, false, true),
            (Asked::Json, false, &read, true, true),
            (Asked::Default, true, &Verb::Help(None), false, true),
            (Asked::Default, true, &Verb::Version, false, true),
            (Asked::Default, true, &Verb::Skill, false, true),
            (
                Asked::Default,
                true,
                &Verb::Run(RunArgs::default()),
                true,
                true,
            ),
        ];
        for (index, (asked, piped, verb, results_json, errors_json)) in rows.into_iter().enumerate()
        {
            assert_eq!(
                formats(asked, piped, verb),
                Formats {
                    results_json,
                    errors_json
                },
                "row {index}"
            );
        }
    }

    // catches: dshd's --patch dropped, reordered, read from dsh's argv after --, or an empty value accepted
    #[test]
    fn run_patch_flag_parses_repeatable_in_order() {
        let Verb::Run(args) = parse(os(&[
            "run",
            "--patch",
            "b.yml",
            "--name",
            "a",
            "--patch=/etc/a.yml",
            "--",
            "web",
            "--patch",
            "c.yml",
        ]))
        .unwrap() else {
            panic!("not run");
        };
        assert_eq!(
            args.patches,
            [PathBuf::from("b.yml"), PathBuf::from("/etc/a.yml")]
        );
        assert_eq!(args.argv, os(&["web", "--patch", "c.yml"]));
        let Verb::Run(none) = parse(os(&["run", "--", "web"])).unwrap() else {
            panic!("not run");
        };
        assert!(none.patches.is_empty());
        for refused in [
            &["run", "--patch", "", "--", "web"][..],
            &["run", "--patch=", "--", "web"],
            &["run", "--patch"],
            &["start", "--patch", "x", "a"],
        ] {
            let error = parse(os(refused)).unwrap_err();
            assert_eq!(error.kind(), "invalid-argument", "{refused:?}: {error}");
        }
    }

    // catches: --json taken from dsh's own argv, or --json with --text accepted
    #[test]
    fn asked_reads_before_double_dash_only() {
        assert_eq!(asked(&os(&["ls"])).unwrap(), Asked::Default);
        assert_eq!(asked(&os(&["--json", "get", "a"])).unwrap(), Asked::Json);
        assert_eq!(asked(&os(&["get", "--text", "a"])).unwrap(), Asked::Text);
        assert_eq!(
            asked(&os(&["run", "--", "web", "--json"])).unwrap(),
            Asked::Default
        );
        assert_eq!(
            asked(&os(&["run", "--text", "--", "web", "--json"])).unwrap(),
            Asked::Text
        );
        assert_eq!(
            asked(&os(&["ls", "--json", "--text"]))
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
