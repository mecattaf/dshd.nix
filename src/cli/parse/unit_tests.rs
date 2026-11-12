//! Unit tests of the parser: every verb and flag, the dsh argv kept
//! verbatim, and the refusals that are exit 2.

use super::*;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn verb(args: &[&str]) -> Verb {
    parse(os(args)).unwrap()
}

fn name(text: &str) -> InstanceName {
    InstanceName::parse(text).unwrap()
}

fn exit(args: &[&str]) -> u8 {
    parse(os(args)).unwrap_err().exit_code()
}

fn kind(args: &[&str]) -> &'static str {
    parse(os(args)).unwrap_err().kind()
}

// catches: launch flags lost, misassigned or given the wrong defaults
#[test]
fn run_arguments_parse() {
    let run = RunArgs {
        name: Some(name("review")),
        release: Some("candidate".into()),
        cwd: Some(PathBuf::from("/srv/work")),
        labels: BTreeMap::from([("agency".to_owned(), "/srv/a".to_owned())]),
        bg: true,
        argv: os(&["acp", "--patch", "p.yml"]),
        ..RunArgs::default()
    };
    let created = [
        "run",
        "--bg",
        "--name",
        "review",
        "--release",
        "candidate",
        "--cwd",
        "/srv/work",
        "--label",
        "agency=/srv/a",
        "--",
        "acp",
        "--patch",
        "p.yml",
    ];
    assert_eq!(verb(&created), Verb::Run(run));
    let headless = RunArgs {
        timeout: 30,
        argv: os(&["headless", "task"]),
        ..RunArgs::default()
    };
    assert_eq!(
        verb(&["run", "--timeout", "30", "--", "headless", "task"]),
        Verb::Run(headless)
    );
    assert_eq!(
        verb(&["run", "--", "web"]),
        Verb::Run(RunArgs {
            argv: os(&["web"]),
            ..RunArgs::default()
        })
    );
}

// catches: lifecycle names, rerun or restart flags parsed as the wrong action
#[test]
fn lifecycle_arguments_parse() {
    assert_eq!(
        verb(&["start", "web-1"]),
        Verb::Start(StartArgs {
            name: name("web-1"),
            rerun: false
        })
    );
    assert_eq!(
        verb(&["start", "--rerun", "batch"]),
        Verb::Start(StartArgs {
            name: name("batch"),
            rerun: true
        })
    );
    let one = |n: &str| NameArgs { name: name(n) };
    assert_eq!(verb(&["stop", "a"]), Verb::Stop(one("a")));
    assert_eq!(verb(&["restart", "a"]), Verb::Restart(one("a")));
    assert_eq!(verb(&["get", "a"]), Verb::Get(one("a")));
    assert_eq!(verb(&["enable", "a"]), Verb::Enable(one("a")));
    assert_eq!(verb(&["disable", "a"]), Verb::Disable(one("a")));
    assert_eq!(verb(&["rm", "a"]), Verb::Rm(one("a")));
    assert_eq!(
        verb(&["update", "-R", "a"]),
        Verb::Update(UpdateArgs {
            name: name("a"),
            restart: true
        })
    );
    assert_eq!(
        verb(&["update", "a"]),
        Verb::Update(UpdateArgs {
            name: name("a"),
            restart: false
        })
    );
    assert_eq!(verb(&["ls"]), Verb::Ls(LsArgs { bare: false }));
    assert_eq!(verb(&[]), Verb::Ls(LsArgs { bare: true }));
    assert_eq!(verb(&["--json"]), Verb::Ls(LsArgs { bare: true }));
}

// catches: read, wait or attachment options lost or assigned to the wrong field
#[test]
fn read_wait_attach_arguments_parse() {
    assert_eq!(
        verb(&["read", "a"]),
        Verb::Read(ReadArgs {
            name: name("a"),
            lines: READ_DEFAULT_LINES,
            head: false,
            follow: false
        })
    );
    assert_eq!(
        verb(&["read", "-f", "-n", "5", "a"]),
        Verb::Read(ReadArgs {
            name: name("a"),
            lines: 5,
            head: false,
            follow: true
        })
    );
    assert_eq!(
        verb(&["read", "--lines=7", "a"]),
        Verb::Read(ReadArgs {
            name: name("a"),
            lines: 7,
            head: false,
            follow: false
        })
    );
    assert_eq!(
        verb(&["wait", "a"]),
        Verb::Wait(WaitArgs {
            name: name("a"),
            timeout: DEFAULT_TIMEOUT_SECS
        })
    );
    assert_eq!(
        verb(&["wait", "--timeout", "0", "a"]),
        Verb::Wait(WaitArgs {
            name: name("a"),
            timeout: 0
        })
    );
    let attach = AttachArgs {
        name: name("a"),
        via: Some(Transport::Acp),
        open: false,
        print_url: false,
        takeover: true,
    };
    assert_eq!(
        verb(&["attach", "--via", "acp", "--takeover", "a"]),
        Verb::Attach(attach)
    );
    let web = AttachArgs {
        name: name("a"),
        via: None,
        open: false,
        print_url: true,
        takeover: false,
    };
    assert_eq!(verb(&["attach", "--print-url", "a"]), Verb::Attach(web));
}

// catches: --head read as the newest lines, combined with -n or --follow, or
// taken by a verb other than read
#[test]
fn read_head_parses() {
    assert_eq!(
        verb(&["read", "--head", "50", "a"]),
        Verb::Read(ReadArgs {
            name: name("a"),
            lines: 50,
            head: true,
            follow: false
        })
    );
    assert_eq!(
        verb(&["read", "--json", "--head=0", "a"]),
        Verb::Read(ReadArgs {
            name: name("a"),
            lines: 0,
            head: true,
            follow: false
        })
    );
    for (args, message) in [
        (
            &["read", "--head", "5", "-n", "5", "a"][..],
            "-n and --head exclude each other",
        ),
        (
            &["read", "-f", "--head", "5", "a"],
            "--head and --follow exclude each other",
        ),
        (
            &["read", "--head", "first", "a"],
            "--head takes a number of lines, not first",
        ),
        (
            &["read", "--head", "1", "--head", "2", "a"],
            "--head given twice",
        ),
        (&["get", "--head", "5", "a"], "get does not take --head"),
    ] {
        let error = parse(os(args)).unwrap_err();
        assert_eq!(error.kind(), "invalid-argument", "{args:?}");
        assert_eq!(error.to_string(), message, "{args:?}");
    }
    assert_eq!(exit(&["read", "--head", "a"]), 2);
}

// catches: labels, help or agent discovery parsed as the wrong verb or arguments
#[test]
fn labels_and_discovery_arguments_parse() {
    let labels = BTreeMap::from([
        ("k".to_owned(), "v".to_owned()),
        ("gone".to_owned(), String::new()),
    ]);
    assert_eq!(
        verb(&["label", "a", "k=v", "gone="]),
        Verb::Label(LabelArgs {
            name: name("a"),
            labels
        })
    );
    assert_eq!(verb(&["help"]), Verb::Help(None));
    assert_eq!(verb(&["help", "read"]), Verb::Help(Some("read".into())));
    assert_eq!(verb(&["stop", "--help"]), Verb::Help(Some("stop".into())));
    assert_eq!(verb(&["-h"]), Verb::Help(None));
    assert_eq!(verb(&["--version"]), Verb::Version);
    assert_eq!(verb(&["-V"]), Verb::Version);
    assert_eq!(verb(&["--skill"]), Verb::Skill);
}

// catches: --json, --help or a NAME after -- taken out of dsh's own argv
#[test]
fn dsh_argv_is_verbatim() {
    let argv = os(&["web", "--json", "--help", "-n", "x"]);
    let parsed = verb(&["run", "--json", "--", "web", "--json", "--help", "-n", "x"]);
    assert_eq!(
        parsed,
        Verb::Run(RunArgs {
            argv,
            ..RunArgs::default()
        })
    );
}

// catches: a bad flag, a flag on the wrong verb or a stray word accepted instead of exit 2
#[test]
fn unknown_flag_is_exit_2() {
    for args in [
        &["stop", "--frob", "a"][..],
        &["stop", "-x", "a"],
        &["frob"],
        &["stop", "--via", "web", "a"],
        &["stop", "a", "b"],
        &["stop", "a", "--", "x"],
        &["ls", "a"],
        &["read", "-n", "many", "a"],
        &["read", "--text", "a"],
        &["read", "--json", "-f", "a"],
        &["attach", "--text", "a"],
        &["help", "--json"],
        &["help", "--text"],
        &["stop", "--rerun", "a"],
        &["run", "--rerun", "--", "web"],
        &["run"],
        &["run", "a", "--", "web"],
        &["run", "--name", "a", "--name", "b", "--", "web"],
        &["run", "--name", "Web", "--", "web"],
        &["run", "--bg", "--timeout", "5", "--", "headless"],
        &["run", "--timeout", "soon", "--", "headless"],
        &["attach", "--via", "tty", "a"],
        &["attach", "--open", "--print-url", "a"],
        &["label", "a"],
        &["label", "a", "k=1", "k=2"],
        &["help", "frob"],
        &["help", "status"],
        &["--version", "ls"],
        &["--version", "--json"],
        &["--skill", "--text"],
        &["--", "web"],
        &["--name", "a"],
        &["list"],
        &["status", "a"],
        &["logs", "a"],
        &["connect", "a"],
        &["remove", "a"],
    ] {
        assert_eq!(exit(args), 2, "{args:?}");
    }
    assert_eq!(
        parse(os(&["stop", "--frob", "a"])).unwrap_err().to_string(),
        "unknown flag --frob"
    );
    assert_eq!(
        parse(os(&["stop", "--via", "web", "a"]))
            .unwrap_err()
            .to_string(),
        "stop does not take --via"
    );
    assert_eq!(
        parse(os(&["list"])).unwrap_err().to_string(),
        "unknown verb list"
    );
}

// catches: a verb without NAME reported as a plain usage error, or start creating from argv
#[test]
fn missing_name_and_start_with_argv() {
    for verb in [
        "start", "stop", "restart", "update", "get", "read", "wait", "attach", "label", "enable",
        "disable", "rm",
    ] {
        assert_eq!(kind(&[verb]), "needs-target", "{verb}");
    }
    let error = parse(os(&["start", "--", "web", "two words"])).unwrap_err();
    assert_eq!(error.kind(), "invalid-argument");
    assert_eq!(
        error.hint().as_deref(),
        Some("dshd run --bg -- web 'two words'")
    );
}

// catches: a label key outside [a-z0-9._-]+ or a repeated key accepted
#[test]
fn label_key_grammar() {
    let labels = |label: &str| match verb(&["run", "--label", label, "--", "web"]) {
        Verb::Run(args) => args.labels,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        labels("agency=/srv/a").get("agency").map(String::as_str),
        Some("/srv/a")
    );
    assert_eq!(
        labels("domicile.workspace=w-1=x")
            .get("domicile.workspace")
            .map(String::as_str),
        Some("w-1=x")
    );
    assert_eq!(labels("k_0-9=").get("k_0-9").map(String::as_str), Some(""));
    for bad in ["Agency=x", "=x", "novalue", "a b=x", "a/b=x"] {
        assert_eq!(exit(&["run", "--label", bad, "--", "web"]), 2, "{bad}");
        assert_eq!(exit(&["label", "a", bad]), 2, "{bad}");
    }
    assert_eq!(
        exit(&["run", "--label", "a=1", "--label", "a=2", "--", "web"]),
        2
    );
}

// catches: a verb the help or the skill names that the parser does not know, or the reverse
#[test]
fn words_round_trip() {
    for word in Word::ALL {
        assert_eq!(Word::parse(word.text()), Some(word));
        for flag in word.flags() {
            assert!(flag.starts_with("--"), "{flag}");
        }
    }
    assert_eq!(Word::parse("list"), None);
}
