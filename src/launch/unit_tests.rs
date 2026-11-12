//! Unit tests for launch.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::record::{EnvVar, OsText, ReleaseFile};

fn argv(tokens: &[&str]) -> Vec<OsString> {
    tokens.iter().map(OsString::from).collect()
}

fn selected(tokens: &[&str]) -> Result<String, Error> {
    super::selected_profile(&argv(tokens))
}

fn pairs(items: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    items
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: OsText::from(name),
        value: OsText::from(value),
        unrecognized: serde_json::Map::new(),
    }
}

// catches: a profile missed behind a patch flag or an inline value, or one taken from the app's arguments (P1.2)
#[test]
fn selects_the_profile_by_the_launcher_grammar() -> Result<(), Error> {
    assert_eq!(selected(&["web", "--port", "4100"])?, "web");
    assert!(
        selected(&["web", "--patch", "a.yml", "--profile", "x"]).is_err(),
        "a --profile after a patch is still the launcher's, so a second selection"
    );
    assert_eq!(
        selected(&[
            "--patch",
            "a.yml",
            "--profile=acp",
            "--patch=b.yml",
            "acp",
            "-V",
            "--",
            "--profile",
            "x",
        ])?,
        "acp",
        "the launcher flags end at the first other token"
    );
    assert_eq!(
        selected(&["--profile", "headless", "run", "--fast"])?,
        "headless"
    );
    assert_eq!(selected(&["tui", "--resume", "--version"])?, "tui");
    assert_eq!(
        selected(&["web@other"])?,
        "web@other",
        "dsh's own name rules apply"
    );
    assert_eq!(
        selected(&["desktop"])?,
        "desktop",
        "dsh refuses it itself, in its own words"
    );
    let raw = OsString::from_vec(vec![b'p', 0xff]);
    let prompt = OsString::from("a $PATH %s \" ' b\nc");
    assert_eq!(
        super::selected_profile(&[OsString::from("--profile=web"), prompt, raw])?,
        "web"
    );
    Ok(())
}

// catches: a run's --patch overlays lost, reordered, taken from the app's arguments, or missed in their inline form
#[test]
fn selection_keeps_the_patch_overlays_in_order() -> Result<(), Error> {
    let chosen = |tokens: &[&str]| -> Result<(String, Vec<OsString>), Error> {
        let selection = super::selection(&argv(tokens))?;
        Ok((selection.profile, selection.patches))
    };
    assert_eq!(chosen(&["web"])?, ("web".to_owned(), Vec::new()));
    assert_eq!(
        chosen(&["headless", "--patch", "pair.yml", "summarise"])?,
        ("headless".to_owned(), argv(&["pair.yml"]))
    );
    assert_eq!(
        chosen(&[
            "--patch=/a.yml",
            "--profile",
            "worker",
            "--patch",
            "./b.yml",
            "-p",
            "task",
            "--patch",
            "c.yml",
        ])?,
        ("worker".to_owned(), argv(&["/a.yml", "./b.yml"])),
        "the overlays end with the launcher flags; a later --patch is the app's"
    );
    assert_eq!(
        chosen(&["web", "--", "--patch", "x.yml"])?.1,
        Vec::<OsString>::new(),
        "nothing after `--` is the launcher's"
    );
    let raw = OsString::from_vec(vec![b'p', 0xff]);
    assert_eq!(
        super::selection(&[
            OsString::from("acp"),
            OsString::from("--patch"),
            raw.clone()
        ])?
        .patches,
        vec![raw],
        "a path that is not UTF-8 is kept as it is"
    );
    Ok(())
}

// catches: an argv accepted that would manage plugins, recreate the profile at every start, print and exit, select two
// profiles, or name a profile outside the DSH home's profiles directory (P1.2)
#[test]
fn selection_refusals() -> Result<(), Error> {
    let refused: &[&[&str]] = &[
        &[],
        &["--patch", "a.yml"],
        &["--port", "1"],
        &["plugin", "list"],
        &["web", "--profile", "acp"],
        &["--profile", "a", "--profile=b"],
        &["--from-default-profile", "web"],
        &["rescue", "--from-default-profile=web"],
        &["web", "--dump-config"],
        &["--dump-default-config=x"],
        &["-V"],
        &["web", "--version"],
        &["--profile"],
        &["web", "--patch"],
        &["--profile="],
        &["."],
        &[".."],
        &["a/b"],
        &["--profile", "../web"],
    ];
    for tokens in refused {
        let found = selected(tokens);
        assert!(
            matches!(found, Err(Error::InvalidArgument(_))),
            "{tokens:?} gave {found:?}"
        );
    }
    let raw = [OsString::from_vec(vec![0xff])];
    assert!(matches!(
        super::selected_profile(&raw),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(
        selected(&["--profile", "plugin"])?,
        "plugin",
        "only the first word is reserved"
    );
    Ok(())
}

// catches: an injected flag overriding or duplicating the user's --no-open, --port or --public-url, or landing after `--` (P3.1)
#[test]
fn inject_web_args_respects_user_flags() {
    let mut first = argv(&["web"]);
    assert!(super::inject_web_args(&mut first, None));
    assert_eq!(first, argv(&["web", "--no-open", "--port", "0"]));

    let mut pinned = argv(&["--profile", "web", "--", "--port"]);
    assert!(super::inject_web_args(&mut pinned, Some(4100)));
    assert_eq!(
        pinned,
        argv(&[
            "--profile",
            "web",
            "--no-open",
            "--port",
            "4100",
            "--",
            "--port"
        ])
    );

    let given: [(&[&str], bool); 5] = [
        (&["--port", "0"], false),
        (&["--port=9"], false),
        (&["--public-url", "https://x"], false),
        (&["--public-url=https://x"], false),
        (&["--no-open"], true),
    ];
    for (user, port_injected) in given {
        let mut args = argv(&["web"]);
        args.extend(argv(user));
        let before = args.clone();
        assert_eq!(
            super::inject_web_args(&mut args, Some(4100)),
            port_injected,
            "{user:?}"
        );
        assert!(args.starts_with(&before), "{user:?}: {args:?}");
        assert_eq!(
            args.iter().filter(|arg| *arg == "--no-open").count(),
            1,
            "{user:?}: {args:?}"
        );
        assert!(
            args.iter().filter(|arg| *arg == "--port").count() <= 1,
            "{user:?}: {args:?}"
        );
    }
}

// catches: a systemd, SSH, login-session or shell variable, another supervisor's identity or DSH_PROFILE, a
// session or desktop variable, or an unsetEnvironment name captured into the record (P1.2, P4.2)
#[test]
fn capture_drops_closed_list() {
    let mut env: Vec<(OsString, OsString)> = super::CAPTURE_EXCLUDED
        .iter()
        .chain(super::SESSION_REFRESHED)
        .map(|name| (OsString::from(name), OsString::from("x")))
        .collect();
    env.extend(pairs(&[("ZED", "z"), ("PLANTED_KEY", "secret")]));
    env.push((OsString::from("ALPHA"), OsString::from_vec(vec![0xff])));
    let captured = super::capture_environment(&env, &["PLANTED_KEY".to_owned()]);
    let alpha = EnvVar {
        name: OsText::from("ALPHA"),
        value: OsText::new(OsString::from_vec(vec![0xff])),
        unrecognized: serde_json::Map::new(),
    };
    assert_eq!(captured, vec![alpha, var("ZED", "z")]);
}

// catches: a step out of order: a stale session or desktop variable kept, a stripped name back, the
// release default beating the record, or DSH_HOME, PWD and the supervisor's variables not last (P1.2, P2.6, P4.2)
#[test]
fn compose_order() {
    let record = [
        var("DISPLAY", ":stale"),
        var("DSH_PERMISSION_MODE", "workspace-write"),
        var("DSH_HOME", "/elsewhere"),
        var("NOTIFY_SOCKET", "/x/notify"),
        var("PROVIDER_KEY", "k"),
        var("TERM", "dumb"),
        var("XDG_CURRENT_DESKTOP", "stale"),
    ];
    let manager = pairs(&[
        ("WAYLAND_DISPLAY", "wayland-1"),
        ("XDG_CURRENT_DESKTOP", "niri"),
        ("XDG_RUNTIME_DIR", "/r/1000"),
        ("PROVIDER_KEY", "m"),
    ]);
    let release = ReleaseFile {
        environment: BTreeMap::from([
            (
                "DSH_PERMISSION_MODE".to_owned(),
                "danger-full-access".to_owned(),
            ),
            ("EXTRA".to_owned(), "release".to_owned()),
        ]),
        unset_environment: vec!["PROVIDER_KEY".to_owned()],
        unrecognized: serde_json::Map::new(),
    };
    let runner = pairs(&[("INVOCATION_ID", "abc"), ("WATCHDOG_USEC", "1")]);
    let composed = super::compose_environment(
        &record,
        &manager,
        &release,
        Path::new("/d/.dsh"),
        Path::new("/w"),
        &runner,
    );
    let expected = pairs(&[
        ("DSH_HOME", "/d/.dsh"),
        ("DSH_PERMISSION_MODE", "workspace-write"),
        ("EXTRA", "release"),
        ("INVOCATION_ID", "abc"),
        ("PWD", "/w"),
        ("TERM", "dumb"),
        ("WAYLAND_DISPLAY", "wayland-1"),
        ("XDG_CURRENT_DESKTOP", "niri"),
        ("XDG_RUNTIME_DIR", "/r/1000"),
    ]);
    assert_eq!(composed, expected);

    let bare = super::compose_environment(
        &record,
        &[],
        &release,
        Path::new("/d"),
        Path::new("/w"),
        &[],
    );
    assert!(bare.contains(&(
        OsString::from("DSH_PERMISSION_MODE"),
        OsString::from("workspace-write")
    )));
    let refreshed = |name: &OsString| {
        name == "INVOCATION_ID" || name == "XDG_RUNTIME_DIR" || name == "XDG_CURRENT_DESKTOP"
    };
    assert!(
        !bare.iter().any(|(name, _)| refreshed(name)),
        "a manager without them leaves them unset: {bare:?}"
    );
    let empty =
        super::compose_environment(&[], &[], &release, Path::new("/d"), Path::new("/w"), &[]);
    assert!(empty.contains(&(
        OsString::from("DSH_PERMISSION_MODE"),
        OsString::from("danger-full-access")
    )));
}

// catches: another supervisor's identity carried into a record, so its crash sweep ends our processes, or
// DSH_PROFILE persisted by a run from inside an instance's shell; and either kept from an older record
#[test]
fn supervisor_and_profile_identity_never_survive() {
    let env = pairs(&[
        ("DSH_PROFILE", "web"),
        ("HC_RUNTIME", "/r/hc"),
        ("HC_SCOPE", "scope-1"),
        ("HC_TERMINAL_ID", "t-1"),
        ("KEEP", "k"),
    ]);
    assert_eq!(
        super::capture_environment(&env, &[]),
        vec![var("KEEP", "k")]
    );
    let older = [
        var("DSH_PROFILE", "web"),
        var("HC_SCOPE", "scope-1"),
        var("KEEP", "k"),
    ];
    let release = ReleaseFile {
        environment: BTreeMap::new(),
        unset_environment: Vec::new(),
        unrecognized: serde_json::Map::new(),
    };
    let composed =
        super::compose_environment(&older, &[], &release, Path::new("/d"), Path::new("/w"), &[]);
    assert!(composed.contains(&(OsString::from("KEEP"), OsString::from("k"))));
    assert!(
        !composed
            .iter()
            .any(|(name, _)| name == "DSH_PROFILE" || name == "HC_SCOPE"),
        "{composed:?}"
    );
}

// catches: the client's DSH_HOME ignored, `~` left unexpanded, a relative value resolved against the
// wrong directory, or a blank value used as a path (P2.6)
#[test]
fn resolve_dsh_home_forms() {
    let home = Path::new("/u");
    let cwd = Path::new("/w/project");
    let with = |value: OsString| {
        super::resolve_dsh_home(
            &BTreeMap::from([(OsString::from("DSH_HOME"), value)]),
            home,
            cwd,
        )
    };
    assert_eq!(
        super::resolve_dsh_home(&BTreeMap::new(), home, cwd),
        Path::new("/u/.dsh")
    );
    assert_eq!(with(OsString::new()), Path::new("/u/.dsh"));
    assert_eq!(with(OsString::from(" \t")), Path::new("/u/.dsh"));
    assert_eq!(with(OsString::from("~")), Path::new("/u"));
    assert_eq!(
        with(OsString::from(["~", ".alt"].join("/"))),
        Path::new("/u/.alt")
    );
    assert_eq!(
        with(OsString::from("rel/dsh")),
        Path::new("/w/project/rel/dsh")
    );
    assert_eq!(with(OsString::from("/abs/dsh")), Path::new("/abs/dsh"));
    assert_eq!(
        with(OsString::from("~other")),
        Path::new("/w/project/~other"),
        "only `~` and `~/` expand"
    );
}

// catches: SS03 another unit's state/credential, trigger, monitor or service-result metadata is captured or replayed
#[test]
fn per_service_metadata_is_never_captured_or_composed() {
    let names = [
        "STATE_DIRECTORY",
        "CACHE_DIRECTORY",
        "LOGS_DIRECTORY",
        "CONFIGURATION_DIRECTORY",
        "CREDENTIALS_DIRECTORY",
        "FDSTORE",
        "MAINPID",
        "PIDFILE",
        "LOG_NAMESPACE",
        "SERVICE_RESULT",
        "EXIT_CODE",
        "EXIT_STATUS",
        "MONITOR_SERVICE_RESULT",
        "MONITOR_EXIT_CODE",
        "MONITOR_EXIT_STATUS",
        "MONITOR_INVOCATION_ID",
        "MONITOR_UNIT",
        "TRIGGER_UNIT",
        "TRIGGER_PATH",
        "TRIGGER_TIMER_REALTIME_USEC",
        "TRIGGER_TIMER_MONOTONIC_USEC",
        "REMOTE_ADDR",
        "REMOTE_PORT",
    ];
    let captured: Vec<_> = names
        .iter()
        .map(|name| (OsString::from(name), OsString::from("stale")))
        .collect();
    assert!(super::capture_environment(&captured, &[]).is_empty());
    let recorded: Vec<_> = names.iter().map(|name| var(name, "stale")).collect();
    let release = ReleaseFile {
        environment: names
            .iter()
            .map(|name| ((*name).to_owned(), "stale".to_owned()))
            .collect(),
        unset_environment: Vec::new(),
        unrecognized: serde_json::Map::new(),
    };
    let composed = super::compose_environment(
        &recorded,
        &[],
        &release,
        Path::new("/home"),
        Path::new("/work"),
        &[],
    );
    for name in names {
        assert!(!composed.iter().any(|(key, _)| key == name), "{name}");
    }
    assert_eq!(
        super::capture_environment(&pairs(&[("MONITOR_CUSTOM", "kept")]), &[]),
        vec![var("MONITOR_CUSTOM", "kept")]
    );
}

// catches: a snapshot overlay given to dsh anywhere but right after the profile, out of the recorded order, after
// an overlay the user typed (which must win), or a typed overlay moved; also with the web flags injected after
#[test]
fn launch_inserts_patches_after_profile_word() {
    let snapshots = [PathBuf::from("/s/00-a.yml"), PathBuf::from("/s/01-b.yml")];
    let ours = ["--patch", "/s/00-a.yml", "--patch", "/s/01-b.yml"];
    let with = |head: &[&str], tail: &[&str]| -> Vec<OsString> {
        head.iter()
            .chain(&ours)
            .chain(tail)
            .map(OsString::from)
            .collect()
    };
    let rows: [(&[&str], Vec<OsString>); 6] = [
        (&["headless", "-p", "t"], with(&["headless"], &["-p", "t"])),
        (
            &["headless", "--patch", "u.yml", "t"],
            with(&["headless"], &["--patch", "u.yml", "t"]),
        ),
        (
            &["--profile", "web", "--patch", "z"],
            with(&["--profile", "web"], &["--patch", "z"]),
        ),
        (
            &["--profile=web", "--patch=z", "--", "x"],
            with(&["--profile=web"], &["--patch=z", "--", "x"]),
        ),
        (
            &["--patch", "z", "--profile", "web"],
            with(&[], &["--patch", "z", "--profile", "web"]),
        ),
        (&["acp"], with(&["acp"], &[])),
    ];
    for (typed, want) in rows {
        let mut args = argv(typed);
        super::insert_patches(&mut args, &snapshots);
        assert_eq!(args, want, "{typed:?}");
        let mut unchanged = argv(typed);
        super::insert_patches(&mut unchanged, &[]);
        assert_eq!(unchanged, argv(typed));
    }
    let mut web = argv(&["--profile", "web", "--patch", "z"]);
    super::insert_patches(&mut web, &snapshots);
    super::inject_web_args(&mut web, None);
    assert_eq!(
        web,
        with(
            &["--profile", "web"],
            &["--patch", "z", "--no-open", "--port", "0"]
        )
    );
}
