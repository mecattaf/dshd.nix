//! The lexopt parser: `dshd VERB [FLAGS] [NAME] [-- DSH ARGUMENTS]`.
//! Unknown verbs and flags are `invalid-argument` (exit 2), a verb without
//! its NAME is `needs-target`; `--help` anywhere before `--` is
//! `Verb::Help`. The format flags are read by `cli::asked`; here they are
//! only checked against the verb.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use lexopt::{Arg, Parser};

use super::render;
use crate::cli::{
    AttachArgs, DEFAULT_TIMEOUT_SECS, LabelArgs, LsArgs, NameArgs, READ_DEFAULT_LINES, ReadArgs,
    RunArgs, StartArgs, UpdateArgs, Verb, WaitArgs,
};
use crate::error::Error;
use crate::name::InstanceName;
use crate::record::{OsText, Transport};

/// The separator between dshd's flags and the native dsh argv.
const SEPARATOR: &str = "--";

/// The verbs, closed. The CLI has no aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Word {
    Run,
    Start,
    Stop,
    Restart,
    Update,
    Ls,
    Get,
    Read,
    Wait,
    Attach,
    Label,
    Enable,
    Disable,
    Rm,
    Help,
}

impl Word {
    /// Every verb, in the order the general help lists them.
    pub(super) const ALL: [Self; 15] = [
        Self::Run,
        Self::Start,
        Self::Stop,
        Self::Restart,
        Self::Update,
        Self::Ls,
        Self::Get,
        Self::Read,
        Self::Wait,
        Self::Attach,
        Self::Label,
        Self::Enable,
        Self::Disable,
        Self::Rm,
        Self::Help,
    ];

    /// The verb as typed.
    pub(super) fn text(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Update => "update",
            Self::Ls => "ls",
            Self::Get => "get",
            Self::Read => "read",
            Self::Wait => "wait",
            Self::Attach => "attach",
            Self::Label => "label",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Rm => "rm",
            Self::Help => "help",
        }
    }

    /// The verb a word names.
    pub(super) fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|verb| verb.text() == word)
    }

    /// The flags the verb accepts, by their long spelling.
    pub(super) fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Run => &[
                "--bg",
                "--name",
                "--release",
                "--cwd",
                "--label",
                "--timeout",
                "--json",
                "--text",
            ],
            Self::Start => &["--rerun", "--json", "--text"],
            Self::Update => &["--restart", "--json", "--text"],
            Self::Read => &["--lines", "--head", "--follow", "--json"],
            Self::Wait => &["--timeout", "--json", "--text"],
            Self::Attach => &["--via", "--open", "--print-url", "--takeover", "--json"],
            Self::Help => &[],
            Self::Stop
            | Self::Restart
            | Self::Ls
            | Self::Get
            | Self::Label
            | Self::Enable
            | Self::Disable
            | Self::Rm => &["--json", "--text"],
        }
    }
}

/// Everything the lexer saw before `--`, before any check against a verb.
#[derive(Debug, Default)]
struct Raw {
    /// The verb and the positionals, in order.
    words: Vec<OsString>,
    /// Long spellings of the flags given, in order.
    seen: Vec<&'static str>,
    name: Option<OsString>,
    release: Option<OsString>,
    cwd: Option<OsString>,
    labels: Vec<OsString>,
    lines: Option<OsString>,
    head: Option<OsString>,
    timeout: Option<OsString>,
    via: Option<OsString>,
}

impl Raw {
    fn has(&self, flag: &str) -> bool {
        self.seen.contains(&flag)
    }

    /// Records a flag; a flag that takes a value may be given once.
    fn mark(&mut self, flag: &'static str, single: bool) -> Result<(), Error> {
        if single && self.has(flag) {
            return Err(Error::InvalidArgument(format!("{flag} given twice")));
        }
        self.seen.push(flag);
        Ok(())
    }
}

/// Parses the process arguments (without `argv[0]`).
pub(super) fn parse(args: Vec<OsString>) -> Result<Verb, Error> {
    let (head, tail) = split(args);
    if head.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Verb::Help(help_topic(&head)));
    }
    build(lex(head)?, tail)
}

/// Splits at the first `--`: what comes after it is dsh's, byte for byte.
fn split(mut args: Vec<OsString>) -> (Vec<OsString>, Option<Vec<OsString>>) {
    match args.iter().position(|arg| arg == SEPARATOR) {
        Some(at) => {
            let tail = args.split_off(at + 1);
            args.truncate(at);
            (args, Some(tail))
        }
        None => (args, None),
    }
}

/// The verb `--help` asks about: the first word, when it is a verb.
fn help_topic(head: &[OsString]) -> Option<String> {
    head.iter()
        .filter_map(|arg| arg.to_str())
        .find(|arg| !arg.starts_with('-'))
        .and_then(Word::parse)
        .map(|word| word.text().to_owned())
}

fn lex(head: Vec<OsString>) -> Result<Raw, Error> {
    let mut parser = Parser::from_args(head);
    let mut raw = Raw::default();
    while let Some(arg) = parser.next().map_err(|e| usage(&e))? {
        let value = |parser: &mut Parser| parser.value().map_err(|e| usage(&e));
        match arg {
            Arg::Long("json") => raw.mark("--json", true)?,
            Arg::Long("text") => raw.mark("--text", true)?,
            Arg::Short('V') | Arg::Long("version") => raw.mark("--version", true)?,
            Arg::Long("skill") => raw.mark("--skill", true)?,
            Arg::Long("bg") => raw.mark("--bg", true)?,
            Arg::Long("rerun") => raw.mark("--rerun", true)?,
            Arg::Long("name") => {
                raw.mark("--name", true)?;
                raw.name = Some(value(&mut parser)?);
            }
            Arg::Long("release") => {
                raw.mark("--release", true)?;
                raw.release = Some(value(&mut parser)?);
            }
            Arg::Long("cwd") => {
                raw.mark("--cwd", true)?;
                raw.cwd = Some(value(&mut parser)?);
            }
            Arg::Long("label") => {
                raw.mark("--label", false)?;
                raw.labels.push(value(&mut parser)?);
            }
            Arg::Long("timeout") => {
                raw.mark("--timeout", true)?;
                raw.timeout = Some(value(&mut parser)?);
            }
            Arg::Short('R') | Arg::Long("restart") => raw.mark("--restart", true)?,
            Arg::Short('f') | Arg::Long("follow") => raw.mark("--follow", true)?,
            Arg::Short('n') | Arg::Long("lines") => {
                raw.mark("--lines", true)?;
                raw.lines = Some(value(&mut parser)?);
            }
            Arg::Long("head") => {
                raw.mark("--head", true)?;
                raw.head = Some(value(&mut parser)?);
            }
            Arg::Long("via") => {
                raw.mark("--via", true)?;
                raw.via = Some(value(&mut parser)?);
            }
            Arg::Long("open") => raw.mark("--open", true)?,
            Arg::Long("print-url") => raw.mark("--print-url", true)?,
            Arg::Long("takeover") => raw.mark("--takeover", true)?,
            Arg::Value(word) => raw.words.push(word),
            Arg::Short(flag) => {
                return Err(Error::InvalidArgument(format!("unknown flag -{flag}")));
            }
            Arg::Long(flag) => {
                return Err(Error::InvalidArgument(format!("unknown flag --{flag}")));
            }
        }
    }
    Ok(raw)
}

fn usage(error: &lexopt::Error) -> Error {
    Error::InvalidArgument(error.to_string())
}

fn build(raw: Raw, tail: Option<Vec<OsString>>) -> Result<Verb, Error> {
    let mut words = raw.words.iter().cloned();
    let Some(first) = words.next() else {
        return bare(&raw, tail.is_some());
    };
    let rest: Vec<OsString> = words.collect();
    let typed = OsText::new(first).display();
    let Some(word) = Word::parse(&typed) else {
        return Err(Error::InvalidArgument(format!("unknown verb {typed}")));
    };
    if let (Word::Start, Some(argv)) = (word, &tail) {
        let shown: Vec<String> = argv
            .iter()
            .map(|arg| render::quoted(&OsText::new(arg.clone()).display()))
            .collect();
        return Err(Error::CreateWithRun(shown.join(" ")));
    }
    if let Some(flag) = raw.seen.iter().find(|flag| !word.flags().contains(*flag)) {
        return Err(Error::InvalidArgument(format!(
            "{typed} does not take {flag}"
        )));
    }
    if tail.is_some() && word != Word::Run {
        return Err(Error::InvalidArgument(format!(
            "{typed} takes no arguments after --; only run does"
        )));
    }
    let verb = word.text();
    let named = |rest| one_name(verb, rest).map(|name| NameArgs { name });
    match word {
        Word::Run => run(raw, rest, tail).map(Verb::Run),
        Word::Start => Ok(Verb::Start(StartArgs {
            name: one_name(verb, rest)?,
            rerun: raw.has("--rerun"),
        })),
        Word::Stop => named(rest).map(Verb::Stop),
        Word::Restart => named(rest).map(Verb::Restart),
        Word::Get => named(rest).map(Verb::Get),
        Word::Enable => named(rest).map(Verb::Enable),
        Word::Disable => named(rest).map(Verb::Disable),
        Word::Rm => named(rest).map(Verb::Rm),
        Word::Update => Ok(Verb::Update(UpdateArgs {
            name: one_name(verb, rest)?,
            restart: raw.has("--restart"),
        })),
        Word::Read => read(&raw, rest).map(Verb::Read),
        Word::Wait => {
            let timeout = seconds(raw.timeout.as_deref())?;
            Ok(Verb::Wait(WaitArgs {
                name: one_name(verb, rest)?,
                timeout,
            }))
        }
        Word::Attach => attach(&raw, rest).map(Verb::Attach),
        Word::Label => label(rest).map(Verb::Label),
        Word::Ls => no_words(verb, rest).map(|()| Verb::Ls(LsArgs { bare: false })),
        Word::Help => help_verb(rest),
    }
}

/// No verb: `--version`, `--skill`, or the list.
fn bare(raw: &Raw, has_tail: bool) -> Result<Verb, Error> {
    if has_tail {
        return Err(Error::InvalidArgument(
            "a verb comes first; only run takes arguments after --".into(),
        ));
    }
    for (flag, verb) in [("--version", Verb::Version), ("--skill", Verb::Skill)] {
        if raw.has(flag) {
            return match raw.seen.iter().find(|seen| **seen != flag) {
                Some(other) => Err(Error::InvalidArgument(format!(
                    "{flag} does not take {other}"
                ))),
                None => Ok(verb),
            };
        }
    }
    match raw
        .seen
        .iter()
        .find(|flag| !matches!(**flag, "--json" | "--text"))
    {
        Some(flag) => Err(Error::InvalidArgument(format!(
            "{flag} needs a verb before it"
        ))),
        None => Ok(Verb::Ls(LsArgs { bare: true })),
    }
}

fn run(raw: Raw, rest: Vec<OsString>, tail: Option<Vec<OsString>>) -> Result<RunArgs, Error> {
    if let Some(word) = rest.into_iter().next() {
        return Err(Error::InvalidArgument(format!(
            "run takes no NAME, got {}; name the instance with --name NAME",
            OsText::new(word).display()
        )));
    }
    let argv = tail.unwrap_or_default();
    if argv.is_empty() {
        return Err(Error::InvalidArgument(
            "run needs -- and the dsh arguments of the instance: dshd run -- web".into(),
        ));
    }
    let bg = raw.has("--bg");
    if bg && raw.timeout.is_some() {
        return Err(Error::InvalidArgument(
            "--timeout bounds a foreground run; --bg returns at once".into(),
        ));
    }
    let release = match raw.release {
        Some(value) => Some(value.into_string().map_err(|value| {
            Error::InvalidArgument(format!(
                "--release {} is not UTF-8",
                OsText::new(value).display()
            ))
        })?),
        None => None,
    };
    Ok(RunArgs {
        name: raw.name.map(name_arg).transpose()?,
        release,
        cwd: raw.cwd.map(PathBuf::from),
        labels: labels("--label ", raw.labels)?,
        bg,
        timeout: seconds(raw.timeout.as_deref())?,
        argv,
    })
}

/// `KEY=VALUE` labels, KEY matching `[a-z0-9._-]+`, each KEY once. An
/// empty VALUE removes the key. `given` prefixes each word in a message.
fn labels(given: &str, words: Vec<OsString>) -> Result<BTreeMap<String, String>, Error> {
    let mut labels = BTreeMap::new();
    for word in words {
        let text = word.into_string().map_err(|word| {
            Error::InvalidArgument(format!(
                "{given}{} is not UTF-8",
                OsText::new(word).display()
            ))
        })?;
        let Some((key, value)) = text.split_once('=') else {
            return Err(Error::InvalidArgument(format!(
                "{given}{text}: expected KEY=VALUE"
            )));
        };
        let key_ok = !key.is_empty()
            && key.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        if !key_ok {
            return Err(Error::InvalidArgument(format!(
                "{given}{text}: KEY must match [a-z0-9._-]+"
            )));
        }
        if labels.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(Error::InvalidArgument(format!("label {key} given twice")));
        }
    }
    Ok(labels)
}

fn label(rest: Vec<OsString>) -> Result<LabelArgs, Error> {
    let mut words = rest.into_iter();
    let Some(name) = words.next() else {
        return Err(Error::NeedsTarget {
            verb: "label",
            candidates: None,
        });
    };
    let name = name_arg(name)?;
    let labels = labels("", words.collect())?;
    if labels.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "label needs KEY=VALUE after {name}; an empty VALUE removes KEY"
        )));
    }
    Ok(LabelArgs { name, labels })
}

fn read(raw: &Raw, rest: Vec<OsString>) -> Result<ReadArgs, Error> {
    let (flag, given) = match (&raw.lines, &raw.head) {
        (Some(_), Some(_)) => {
            return Err(Error::InvalidArgument(
                "-n and --head exclude each other".into(),
            ));
        }
        (Some(value), None) => ("-n", Some(value)),
        (None, Some(value)) => ("--head", Some(value)),
        (None, None) => ("-n", None),
    };
    let lines = match given {
        Some(value) => value
            .to_str()
            .and_then(|text| text.parse::<u32>().ok())
            .ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "{flag} takes a number of lines, not {}",
                    OsText::new(value).display()
                ))
            })?,
        None => READ_DEFAULT_LINES,
    };
    let head = raw.head.is_some();
    let follow = raw.has("--follow");
    if follow && raw.has("--json") {
        return Err(Error::InvalidArgument(
            "read --follow prints text; read --json prints the lines once".into(),
        ));
    }
    if follow && head {
        return Err(Error::InvalidArgument(
            "--head and --follow exclude each other".into(),
        ));
    }
    Ok(ReadArgs {
        name: one_name("read", rest)?,
        lines,
        head,
        follow,
    })
}

/// `--timeout S`, whole seconds; the default when absent.
fn seconds(value: Option<&OsStr>) -> Result<u64, Error> {
    match value {
        None => Ok(DEFAULT_TIMEOUT_SECS),
        Some(value) => value
            .to_str()
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "--timeout takes whole seconds, not {}",
                    OsText::new(value).display()
                ))
            }),
    }
}

fn attach(raw: &Raw, rest: Vec<OsString>) -> Result<AttachArgs, Error> {
    let via = match raw.via.as_deref().map(OsStr::to_str) {
        None => None,
        Some(Some("web")) => Some(Transport::Web),
        Some(Some("acp")) => Some(Transport::Acp),
        Some(_) => return Err(Error::InvalidArgument("--via takes web or acp".into())),
    };
    let args = AttachArgs {
        name: one_name("attach", rest)?,
        via,
        open: raw.has("--open"),
        print_url: raw.has("--print-url"),
        takeover: raw.has("--takeover"),
    };
    if args.open && args.print_url {
        return Err(Error::InvalidArgument(
            "--open and --print-url exclude each other".into(),
        ));
    }
    Ok(args)
}

fn help_verb(rest: Vec<OsString>) -> Result<Verb, Error> {
    match at_most_one("help", rest)? {
        None => Ok(Verb::Help(None)),
        Some(word) => {
            let topic = OsText::new(word).display();
            match Word::parse(&topic) {
                Some(verb) => Ok(Verb::Help(Some(verb.text().to_owned()))),
                None => Err(Error::InvalidArgument(format!(
                    "no help for {topic}: it is not a verb"
                ))),
            }
        }
    }
}

fn one_name(verb: &'static str, rest: Vec<OsString>) -> Result<InstanceName, Error> {
    match at_most_one(verb, rest)? {
        Some(word) => name_arg(word),
        None => Err(Error::NeedsTarget {
            verb,
            candidates: None,
        }),
    }
}

fn no_words(verb: &str, rest: Vec<OsString>) -> Result<(), Error> {
    match at_most_one(verb, rest)? {
        Some(word) => Err(Error::InvalidArgument(format!(
            "{verb} takes no NAME, got {}",
            OsText::new(word).display()
        ))),
        None => Ok(()),
    }
}

fn at_most_one(verb: &str, rest: Vec<OsString>) -> Result<Option<OsString>, Error> {
    let mut words = rest.into_iter();
    let first = words.next();
    match words.next() {
        Some(extra) => Err(Error::InvalidArgument(format!(
            "{verb} takes one NAME; unexpected {}",
            OsText::new(extra).display()
        ))),
        None => Ok(first),
    }
}

/// A NAME argument. Text that is not UTF-8 is shown with its bytes, which
/// the grammar rejects like any other character outside it.
fn name_arg(word: OsString) -> Result<InstanceName, Error> {
    InstanceName::parse(&OsText::new(word).display())
}

#[cfg(test)]
mod unit_tests;
