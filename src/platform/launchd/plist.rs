//! The one `LaunchAgent` property list dshd writes per instance, from a
//! closed key set. Scheduling is not dshd's job: whatever schedules work
//! starts instances from outside with `dshd start`, and some hosts refuse
//! any job that carries a schedule key. So the renderer writes
//! exactly [`ALLOWED_KEYS`], in that order, and nothing else; no plist
//! ever holds an environment value, a token or a credential.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The keys dshd writes, in the order it writes them. Adding a key is a
/// reviewed change to this list.
pub const ALLOWED_KEYS: &[&str] = &[
    "Label",
    "ProgramArguments",
    "WorkingDirectory",
    "RunAtLoad",
    "LimitLoadToSessionType",
    "ProcessType",
    "ExitTimeOut",
    "AbandonProcessGroup",
    "StandardOutPath",
    "StandardErrorPath",
];

/// Keys dshd never writes: restart policy lives in the runner, the
/// runner composes dsh's environment, and nothing is ever scheduled.
pub const NEVER_WRITTEN: &[&str] = &[
    "KeepAlive",
    "StartInterval",
    "StartCalendarInterval",
    "WatchPaths",
    "QueueDirectories",
    "StartOnMount",
    "Sockets",
    "Disabled",
    "EnvironmentVariables",
];

/// Mode of every plist dshd writes: owned by the user, never group- or
/// world-writable, and holding nothing secret.
pub const PLIST_MODE: u32 = 0o644;

/// What one instance's job says. Everything else is fixed by the renderer:
/// `RunAtLoad` true, `LimitLoadToSessionType` `Aqua`, `ProcessType`
/// `Standard`, `AbandonProcessGroup` false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// `Label`: `dshd.NAME`.
    pub label: String,
    /// The first entry of `ProgramArguments`: `<instance>/current/bin/dshd-run`.
    pub program: PathBuf,
    /// The rest of `ProgramArguments`: `run NAME`.
    pub arguments: Vec<String>,
    /// `WorkingDirectory`: the instance directory.
    pub working_directory: PathBuf,
    /// `StandardOutPath` and `StandardErrorPath`: `<instance>/launchd.log`.
    pub log_path: PathBuf,
    /// `ExitTimeOut` in seconds.
    pub exit_timeout: u32,
}

/// Renders `spec` as an XML property list: the XML declaration, Apple's
/// PLIST 1.0 doctype, one `<dict>` holding [`ALLOWED_KEYS`] in order, one
/// tab of indentation per level, `&`, `<` and `>` escaped in every string,
/// and a final newline. A path that is not UTF-8 is an `Io` error with
/// action `render`, since a plist string must be text.
pub fn render(spec: &JobSpec) -> Result<String, Error> {
    let program = path_text(&spec.program)?;
    let working_directory = path_text(&spec.working_directory)?;
    let log_path = path_text(&spec.log_path)?;

    let mut xml = String::new();
    emit(
        &mut xml,
        format_args!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
    );
    emit(
        &mut xml,
        format_args!(
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">"
        ),
    );
    emit(&mut xml, format_args!("<plist version=\"1.0\">"));
    emit(&mut xml, format_args!("<dict>"));

    emit(&mut xml, format_args!("\t<key>Label</key>"));
    emit(
        &mut xml,
        format_args!("\t<string>{}</string>", escape(&spec.label)),
    );
    emit(&mut xml, format_args!("\t<key>ProgramArguments</key>"));
    emit(&mut xml, format_args!("\t<array>"));
    emit(
        &mut xml,
        format_args!("\t\t<string>{}</string>", escape(program)),
    );
    for argument in &spec.arguments {
        emit(
            &mut xml,
            format_args!("\t\t<string>{}</string>", escape(argument)),
        );
    }
    emit(&mut xml, format_args!("\t</array>"));
    emit(&mut xml, format_args!("\t<key>WorkingDirectory</key>"));
    emit(
        &mut xml,
        format_args!("\t<string>{}</string>", escape(working_directory)),
    );
    emit(&mut xml, format_args!("\t<key>RunAtLoad</key>"));
    emit(&mut xml, format_args!("\t<true/>"));
    emit(
        &mut xml,
        format_args!("\t<key>LimitLoadToSessionType</key>"),
    );
    emit(&mut xml, format_args!("\t<string>Aqua</string>"));
    emit(&mut xml, format_args!("\t<key>ProcessType</key>"));
    emit(&mut xml, format_args!("\t<string>Standard</string>"));
    emit(&mut xml, format_args!("\t<key>ExitTimeOut</key>"));
    emit(
        &mut xml,
        format_args!("\t<integer>{}</integer>", spec.exit_timeout),
    );
    emit(&mut xml, format_args!("\t<key>AbandonProcessGroup</key>"));
    emit(&mut xml, format_args!("\t<false/>"));
    emit(&mut xml, format_args!("\t<key>StandardOutPath</key>"));
    emit(
        &mut xml,
        format_args!("\t<string>{}</string>", escape(log_path)),
    );
    emit(&mut xml, format_args!("\t<key>StandardErrorPath</key>"));
    emit(
        &mut xml,
        format_args!("\t<string>{}</string>", escape(log_path)),
    );

    emit(&mut xml, format_args!("</dict>"));
    emit(&mut xml, format_args!("</plist>"));
    Ok(xml)
}

/// Appends one line to `xml`. Writing to a `String` cannot fail, so the
/// formatting result is discarded.
fn emit(xml: &mut String, arguments: std::fmt::Arguments<'_>) {
    let _formatted = writeln!(xml, "{arguments}");
}

/// Escapes the XML metacharacters of one plist string.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A plist string must be UTF-8 text.
fn path_text(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or_else(|| {
        Error::io(
            "render",
            path.to_path_buf(),
            std::io::Error::new(std::io::ErrorKind::InvalidData, "path is not valid UTF-8"),
        )
    })
}

/// The keys of the top-level `<dict>` of a plist dshd rendered, in order.
/// Keys of nested dictionaries are not top-level keys.
pub fn top_level_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        let Some(current) = rest.get(start..) else {
            break;
        };
        rest = current;
        if let Some(after) = rest.strip_prefix("<dict>") {
            depth += 1;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("</dict>") {
            depth = depth.saturating_sub(1);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("<key>") {
            let Some(end) = after.find("</key>") else {
                break;
            };
            if depth == 1 {
                keys.extend(after.get(..end).map(str::to_owned));
            }
            let Some(tail) = after.get(end + "</key>".len()..) else {
                break;
            };
            rest = tail;
        } else {
            let Some(tail) = rest.get(1..) else {
                break;
            };
            rest = tail;
        }
    }
    keys
}

/// `Ok` when every top-level key is in [`ALLOWED_KEYS`]; otherwise the
/// first key that is not.
pub fn check_keys(xml: &str) -> Result<(), String> {
    match top_level_keys(xml)
        .into_iter()
        .find(|key| !ALLOWED_KEYS.contains(&key.as_str()))
    {
        Some(key) => Err(key),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "plist_tests.rs"]
mod tests;
