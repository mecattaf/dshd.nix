//! The launch recipe, as pure functions: finding the profile native argv
//! selects by DSH's launcher grammar, the injection of the `--patch`
//! snapshots and of the web flags, environment capture with the closed
//! exclusion list, and the ordered composition of the child environment at
//! every start.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::record::{EnvVar, OsText, ReleaseFile};

/// Never captured at creation nor refreshed: names that describe one
/// process's place rather than the user's setup. Exact names only.
pub const CAPTURE_EXCLUDED: &[&str] = &[
    // Set by systemd for one service.
    "NOTIFY_SOCKET",
    "LISTEN_PID",
    "LISTEN_FDS",
    "LISTEN_FDNAMES",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "MANAGERPID",
    "MANAGERPIDFDID",
    "SYSTEMD_EXEC_PID",
    "RUNTIME_DIRECTORY",
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
    "MEMORY_PRESSURE_WATCH",
    "MEMORY_PRESSURE_WRITE",
    // The SSH connection.
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    // The login session, which the unit is not part of.
    "XDG_SESSION_ID",
    "XDG_SESSION_CLASS",
    "XDG_SEAT",
    "XDG_VTNR",
    // Shell bookkeeping.
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    // Another supervisor's per-process identity: carrying it lets that
    // supervisor end or adopt our processes.
    "HC_SCOPE",
    "HC_RUNTIME",
    "HC_TERMINAL_ID",
    // DSH's own identity, which dsh sets in the shells of the profile it
    // launched.
    "DSH_PROFILE",
];

/// Never captured; taken from the user manager at each start, or left
/// unset when the manager has none, never kept from the caller: a retained
/// service outlives the session it was started from. The desktop
/// descriptors follow the display variables they describe.
pub const SESSION_REFRESHED: &[&str] = &[
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "SSH_AUTH_SOCK",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
];

/// Passed through from the runner's own environment at the end of
/// composition, so DSH recognises systemd supervision.
pub const RUNNER_PASSTHROUGH: &[&str] = &["INVOCATION_ID", "JOURNAL_STREAM"];

/// The profile word DSH's launcher reserves for plugin management.
const REFUSED_PROFILE_WORD: &str = "plugin";

/// Launcher flags that take a value, as `FLAG V` or `FLAG=V`.
const PROFILE_FLAG: &str = "--profile";
const PATCH_FLAG: &str = "--patch";
/// The launcher flag that recreates the profile from a shipped template.
const FROM_DEFAULT_PROFILE_FLAG: &str = "--from-default-profile";
/// Launcher flags that print and exit.
const VERSION_FLAGS: &[&str] = &["-V", "--version"];
/// Prefix of the launcher's dump flags, which print and exit.
const DUMP_PREFIX: &str = "--dump-";
/// Ends option parsing; nothing after it is a flag.
const END_OF_FLAGS: &str = "--";
/// The injected web flags.
const NO_OPEN_FLAG: &str = "--no-open";
const PORT_FLAG: &str = "--port";
const PUBLIC_URL_FLAG: &str = "--public-url";
/// The port of a first start: the kernel picks one and DSH announces it.
const FIRST_START_PORT: u16 = 0;
/// The variable naming DSH's home.
pub(crate) const DSH_HOME: &str = "DSH_HOME";
/// The variable naming the working directory.
const PWD: &str = "PWD";
/// DSH's home under the user's home when `DSH_HOME` is blank.
const DEFAULT_DSH_HOME: &str = ".dsh";

/// What native argv selects, by DSH's launcher grammar: the profile and the
/// `--patch` overlays DSH applies after the profile layer, in argv order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The profile name.
    pub profile: String,
    /// The `--patch` values as typed; DSH resolves them against the
    /// instance's working directory at every start.
    pub patches: Vec<OsString>,
}

/// The profile native argv selects. See [`selection`].
pub fn selected_profile(argv: &[OsString]) -> Result<String, Error> {
    selection(argv).map(|selection| selection.profile)
}

/// The profile and the patch overlays native argv selects, by DSH's
/// launcher grammar: a first token not starting with `-` is the profile
/// word; otherwise the leading `--profile V|--profile=V` and
/// `--patch V|--patch=V` flags are scanned up to the first other token,
/// after the profile word too. The argv itself is dsh's, recorded as typed.
///
/// Refuses (invalid-argument) what cannot be a held session: no profile,
/// a profile selected twice, the word `plugin`, a profile that is not one
/// directory name, `--from-default-profile`, and the flags that print and
/// exit (`--dump-*`, `-V`, `--version`).
pub fn selection(argv: &[OsString]) -> Result<Selection, Error> {
    let mut profile = None;
    let mut patches = Vec::new();
    let mut index = 0;
    if let Some(word) = argv
        .first()
        .filter(|token| !token.as_bytes().starts_with(b"-"))
    {
        if word == REFUSED_PROFILE_WORD {
            return Err(refuse(format!(
                "`{REFUSED_PROFILE_WORD}` manages DSH plugins and cannot be an instance"
            )));
        }
        profile = Some(profile_name(word)?);
        index = 1;
    }
    while let Some(token) = argv.get(index) {
        let (flag, inline) = split_flag(token);
        let Some(flag @ (PROFILE_FLAG | PATCH_FLAG)) = flag else {
            if let Some(flag) = flag {
                refuse_flag(flag)?;
            }
            break;
        };
        let value = if let Some(value) = inline {
            value
        } else {
            index += 1;
            argv.get(index)
                .ok_or_else(|| refuse(format!("{flag} needs a value")))?
                .as_os_str()
        };
        if flag == PROFILE_FLAG {
            if profile.is_some() {
                return Err(refuse("the profile is selected twice".to_owned()));
            }
            profile = Some(profile_name(value)?);
        } else {
            patches.push(value.to_owned());
        }
        index += 1;
    }
    let profile = profile
        .ok_or_else(|| refuse("no profile: give a profile word or --profile NAME".to_owned()))?;
    Ok(Selection { profile, patches })
}

/// Inserts `--patch SNAPSHOT` for each snapshot, in order, directly after
/// the profile the argv selects (the profile word, or `--profile V`), so
/// DSH applies them after the profile's own layer and before every
/// `--patch` typed in the argv: the typed overlays win. When a typed
/// `--patch` comes before `--profile V`, the snapshots go before it, for
/// the same reason. Done at every start, like the web flags.
pub fn insert_patches(args: &mut Vec<OsString>, snapshots: &[PathBuf]) {
    if snapshots.is_empty() {
        return;
    }
    let rest = args.split_off(patch_position(args).min(args.len()));
    args.extend(
        snapshots
            .iter()
            .flat_map(|snapshot| [OsString::from(PATCH_FLAG), snapshot.as_os_str().to_owned()]),
    );
    args.extend(rest);
}

/// Where [`insert_patches`] puts the snapshots: after the profile word,
/// or after `--profile V` unless a typed `--patch` comes first.
fn patch_position(args: &[OsString]) -> usize {
    if args
        .first()
        .is_some_and(|token| !token.as_bytes().starts_with(b"-"))
    {
        return 1;
    }
    let mut index = 0;
    let mut typed_patch = None;
    while let Some(token) = args.get(index) {
        let (flag, inline) = split_flag(token);
        let width = if inline.is_some() { 1 } else { 2 };
        match flag {
            Some(PROFILE_FLAG) => return typed_patch.unwrap_or(index + width),
            Some(PATCH_FLAG) => {
                typed_patch.get_or_insert(index);
            }
            _ => break,
        }
        index += width;
    }
    typed_patch.unwrap_or(index)
}

/// For a web instance: appends `--no-open` unless present, and `--port N`
/// with the pinned port (or `0` on first start) unless the user gave
/// `--port`. Returns whether a port flag was injected.
///
/// Only the flags before the first `--` count, and the injected ones go
/// there too, after the user's own.
pub fn inject_web_args(args: &mut Vec<OsString>, pinned_port: Option<u16>) -> bool {
    let end = flags_end(args);
    let given = |wanted: &[&str]| {
        args.iter()
            .take(end)
            .any(|arg| split_flag(arg).0.is_some_and(|flag| wanted.contains(&flag)))
    };
    let mut injected = Vec::new();
    if !given(&[NO_OPEN_FLAG]) {
        injected.push(OsString::from(NO_OPEN_FLAG));
    }
    let port = !given(&[PORT_FLAG, PUBLIC_URL_FLAG]);
    if port {
        let value =
            pinned_port.map_or_else(|| FIRST_START_PORT.to_string(), |pinned| pinned.to_string());
        injected.extend([OsString::from(PORT_FLAG), OsString::from(value)]);
    }
    let rest = args.split_off(end);
    args.extend(injected);
    args.extend(rest);
    port
}

/// The CLI's environment minus [`CAPTURE_EXCLUDED`], [`SESSION_REFRESHED`]
/// and `unset`, sorted by name.
pub fn capture_environment(env: &[(OsString, OsString)], unset: &[String]) -> Vec<EnvVar> {
    let mut captured: Vec<EnvVar> = env
        .iter()
        .filter(|(name, _)| {
            !listed(name, CAPTURE_EXCLUDED)
                && !listed(name, SESSION_REFRESHED)
                && !unset.iter().any(|unset| name == unset.as_str())
        })
        .map(|(name, value)| EnvVar {
            name: OsText::new(name.clone()),
            value: OsText::new(value.clone()),
            unrecognized: serde_json::Map::new(),
        })
        .collect();
    captured.sort_by(|left, right| left.name.cmp(&right.name));
    captured
}

/// Composes the child environment in order: the record; the refreshed
/// session variables from `manager` (absent ones left unset); strip
/// `release.unset_environment`; `release.environment` fills names still
/// unset; `DSH_HOME` and `PWD`; then [`RUNNER_PASSTHROUGH`] from `runner`.
///
/// The strip also removes the [`CAPTURE_EXCLUDED`] names, so a record or
/// release never hands dsh the manager's sockets.
pub fn compose_environment(
    record: &[EnvVar],
    manager: &[(OsString, OsString)],
    release: &ReleaseFile,
    dsh_home: &Path,
    cwd: &Path,
    runner: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut env: BTreeMap<OsString, OsString> = record
        .iter()
        .map(|var| {
            (
                var.name.as_os_str().to_owned(),
                var.value.as_os_str().to_owned(),
            )
        })
        .collect();
    for name in SESSION_REFRESHED {
        refresh(&mut env, name, manager);
    }
    for name in release
        .unset_environment
        .iter()
        .map(String::as_str)
        .chain(CAPTURE_EXCLUDED.iter().copied())
    {
        env.remove(OsStr::new(name));
    }
    for (name, value) in &release.environment {
        if listed(OsStr::new(name), CAPTURE_EXCLUDED) {
            continue;
        }
        env.entry(OsString::from(name))
            .or_insert_with(|| OsString::from(value));
    }
    env.insert(OsString::from(DSH_HOME), dsh_home.as_os_str().to_owned());
    env.insert(OsString::from(PWD), cwd.as_os_str().to_owned());
    for name in RUNNER_PASSTHROUGH {
        refresh(&mut env, name, runner);
    }
    env.into_iter().collect()
}

/// The client's `DSH_HOME` when non-blank (`~` expanded, relative resolved
/// against `cwd`), else `<home>/.dsh`.
pub fn resolve_dsh_home(env: &BTreeMap<OsString, OsString>, home: &Path, cwd: &Path) -> PathBuf {
    let Some(value) = env
        .get(OsStr::new(DSH_HOME))
        .filter(|value| !value.as_bytes().iter().all(u8::is_ascii_whitespace))
    else {
        return home.join(DEFAULT_DSH_HOME);
    };
    let bytes = value.as_bytes();
    let path = if bytes == b"~" {
        home.to_path_buf()
    } else if let Some(below) = bytes
        .strip_prefix(b"~")
        .and_then(|rest| rest.strip_prefix(b"/"))
    {
        home.join(OsStr::from_bytes(below))
    } else {
        PathBuf::from(value)
    };
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// A launcher token split into its flag and an inline `=value`. Only a
/// `--` token has an inline value; a flag that is not UTF-8 is no flag.
fn split_flag(token: &OsStr) -> (Option<&str>, Option<&OsStr>) {
    let bytes = token.as_bytes();
    match bytes.iter().position(|&byte| byte == b'=') {
        Some(at) if bytes.starts_with(b"--") => {
            let (flag, value) = bytes.split_at(at);
            (
                OsStr::from_bytes(flag).to_str(),
                value.get(1..).map(OsStr::from_bytes),
            )
        }
        _ => (token.to_str(), None),
    }
}

/// Checks a selected profile name: dshd reads the profile's manifest at
/// `<DSH home>/profiles/<name>`, so it must be one directory name, and
/// UTF-8, since records and generated names hold it as text.
fn profile_name(word: &OsStr) -> Result<String, Error> {
    let Some(profile) = word.to_str() else {
        return Err(refuse(format!(
            "the profile {} is not UTF-8",
            Path::new(word).display()
        )));
    };
    let reason = if profile.is_empty() {
        Some("it is empty")
    } else if matches!(profile, "." | "..") {
        Some("it names a directory")
    } else if profile.contains('/') {
        Some("it contains `/`")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(refuse(format!(
            "the profile {profile:?} cannot be used: {reason}"
        ))),
        None => Ok(profile.to_owned()),
    }
}

/// Refuses a launcher flag that does not start a session dshd can hold.
/// `--from-default-profile` recreates the profile from a shipped template,
/// which a held instance would repeat at every start, discarding what the
/// user and dsh changed in the profile since.
fn refuse_flag(flag: &str) -> Result<(), Error> {
    if flag == FROM_DEFAULT_PROFILE_FLAG {
        Err(refuse(format!(
            "{flag} would recreate the profile from its template at every start of the instance; \
             run it once with dsh directly, then give the profile alone"
        )))
    } else if VERSION_FLAGS.contains(&flag) || flag.starts_with(DUMP_PREFIX) {
        Err(refuse(format!(
            "{flag} prints and exits; run it with dsh directly"
        )))
    } else {
        Ok(())
    }
}

fn refuse(message: String) -> Error {
    Error::InvalidArgument(message)
}

/// The number of tokens before the first `--`.
fn flags_end(args: &[OsString]) -> usize {
    args.iter()
        .take_while(|arg| arg.as_os_str() != END_OF_FLAGS)
        .count()
}

fn listed(name: &OsStr, names: &[&str]) -> bool {
    names.iter().any(|listed| name == *listed)
}

/// Sets `name` from the last occurrence in `source`, or unsets it.
fn refresh(env: &mut BTreeMap<OsString, OsString>, name: &str, source: &[(OsString, OsString)]) {
    match source.iter().rev().find(|(candidate, _)| candidate == name) {
        Some((_, value)) => {
            env.insert(OsString::from(name), value.clone());
        }
        None => {
            env.remove(OsStr::new(name));
        }
    }
}

#[cfg(test)]
#[path = "launch/unit_tests.rs"]
mod tests;
