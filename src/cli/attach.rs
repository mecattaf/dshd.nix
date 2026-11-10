//! `dshd attach NAME`: never starts. Web prints the origin of the
//! announced address from the runtime `web-url` file, the full URL with
//! `--print-url`, or hands it to `xdg-open` with `--open`; acp execs
//! `booted/bin/dshd-run attach NAME [--takeover]` on this terminal. A
//! headless instance has nothing to attach to.

use std::os::unix::process::CommandExt;
use std::process::Stdio;

use serde::Serialize;

use crate::cli::{Asked, AttachArgs, Context};
use crate::error::{Error, ToolFailure};
use crate::fsx;
use crate::instance::{self, Instance};
use crate::name::{self, RuntimeDir};
use crate::out;
use crate::record::{OsText, Transport};
use crate::runner::web;
use crate::systemctl::ActiveState;
use crate::tool::Tool;

/// `attach --json` for a web instance.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebConnection {
    /// Scheme, host and port of the announced address.
    origin: String,
    /// The announced port.
    port: Option<u16>,
    /// The 0600 file that holds the full URL.
    url_file: String,
    /// The full URL, only with `--print-url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

/// Runs the verb. For acp it returns only on failure to exec.
pub(super) fn run(ctx: &Context, args: &AttachArgs) -> Result<(), Error> {
    let instance = instance::load(&ctx.state_root, &args.name)?;
    let recorded = instance.record.transport;
    // `--via` defaults to the recorded transport (the flag's documented default).
    let acp = match (recorded, args.via.unwrap_or(recorded)) {
        (Transport::Web, Transport::Web) => false,
        (Transport::Acp, Transport::Acp) => true,
        (other, _) => {
            return Err(Error::UnsupportedTransport {
                name: args.name.to_string(),
                transport: other.slug(),
            });
        }
    };
    check_flags(args, acp, ctx.asked == Asked::Json)?;
    if ctx.systemctl.show(&args.name.unit())?.active_state != ActiveState::Active {
        return Err(Error::NotRunning(args.name.to_string()));
    }
    if acp {
        Err(attach(ctx, &instance, args.takeover))
    } else {
        attach_web(ctx, args)
    }
}

/// The flags each transport takes. `json` is an explicit `--json`, which
/// acp refuses: its output is the ACP channel itself.
fn check_flags(args: &AttachArgs, acp: bool, json: bool) -> Result<(), Error> {
    let (transport, foreign) = if acp {
        (
            Transport::Acp.slug(),
            vec![
                (args.open, "--open"),
                (args.print_url, "--print-url"),
                (json, "--json"),
            ],
        )
    } else {
        (Transport::Web.slug(), vec![(args.takeover, "--takeover")])
    };
    match foreign.into_iter().find(|(given, _)| *given) {
        Some((_, flag)) => refuse(transport, flag),
        None => Ok(()),
    }
}

/// `invalid-argument` for a flag the transport does not take.
fn refuse(transport: &str, flag: &str) -> Result<(), Error> {
    Err(Error::InvalidArgument(format!(
        "attach over {transport} does not take {flag}"
    )))
}

/// Prints the origin (default), the full URL (`--print-url`), the JSON
/// object (`--json`), or opens the URL (`--open`, which prints nothing
/// but the JSON object).
fn attach_web(ctx: &Context, args: &AttachArgs) -> Result<(), Error> {
    let file = RuntimeDir::under(&ctx.runtime_root, &args.name).web_url();
    let Some(announced) = fsx::read_text(&file)? else {
        return Err(Error::NotRunning(args.name.to_string()));
    };
    let url = announced.trim();
    if args.open {
        open(ctx, url)?;
    }
    if ctx.json {
        return out::json(&WebConnection {
            origin: origin_of(url).to_owned(),
            port: web::port_of(url),
            url_file: OsText::new(file.into_os_string()).display(),
            url: args.print_url.then(|| url.to_owned()),
        });
    }
    if args.open {
        return Ok(());
    }
    if args.print_url {
        return out::line(url);
    }
    out::line(origin_of(url))?;
    out::error_line(&format!(
        "Open it in a browser that already holds its cookie, or print the full URL: dshd attach --print-url {}",
        args.name
    ));
    Ok(())
}

/// Hands the URL to `xdg-open` as one argument, in the client's own
/// environment. Its stdout is discarded so a browser it starts holds no
/// pipe of ours; its stderr is the user's.
fn open(ctx: &Context, url: &str) -> Result<(), Error> {
    let mut command = ctx.xdg_open.command();
    command
        .env_clear()
        .envs(ctx.environment.iter().cloned())
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let status = command
        .status()
        .map_err(|error| ctx.xdg_open.spawn_error(error))?;
    if status.success() {
        return Ok(());
    }
    // The URL carries the instance's secret, so the message names it only.
    Err(Error::Tool(Box::new(ToolFailure {
        tool: ctx.xdg_open.name,
        args: vec!["URL".to_owned()],
        status: status.code(),
        stderr: String::new(),
    })))
}

/// Execs the booted runner's ACP client on this terminal's stdio. Returns
/// the error when the exec failed.
fn attach(ctx: &Context, instance: &Instance, takeover: bool) -> Error {
    let runner = Tool::at("dshd-run", name::booted_runner(&instance.dir));
    let mut command = runner.command();
    command
        .arg("attach")
        .arg(instance.name.as_str())
        .env("XDG_RUNTIME_DIR", &ctx.runtime_root)
        .stdin(Stdio::inherit());
    if takeover {
        command.arg("--takeover");
    }
    runner.spawn_error(command.exec())
}

/// `scheme://authority` of a URL: everything before the first `/`, `?` or
/// `#` after `://`. Text without `://` is returned whole.
fn origin_of(url: &str) -> &str {
    let Some((_, rest)) = url.split_once("://") else {
        return url;
    };
    match rest.find(['/', '?', '#']) {
        // Both offsets are at ASCII bytes, hence at char boundaries.
        Some(end) => url.split_at(url.len() - rest.len() + end).0,
        None => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::InstanceName;

    fn args(via: Option<Transport>, open: bool, print_url: bool, takeover: bool) -> AttachArgs {
        AttachArgs {
            name: InstanceName::parse("web-1").unwrap(),
            via,
            open,
            print_url,
            takeover,
        }
    }

    /// Any port: the origin keeps it whatever it is.
    const PORT: u16 = 41_234;

    // catches: the origin printed with its path and query, which hold the secret
    #[test]
    fn origins() {
        let local = format!("http://host.example:{PORT}");
        assert_eq!(origin_of(&format!("{local}/?key=x")), local);
        assert_eq!(origin_of(&local), local);
        let bracketed = format!("http://[fd00::1]:{PORT}");
        assert_eq!(origin_of(&format!("{bracketed}?key=x")), bracketed);
        assert_eq!(
            origin_of("https://host.example#frag"),
            "https://host.example"
        );
        assert_eq!(origin_of("not a url"), "not a url");
    }

    // catches: a flag of one transport accepted for the other
    #[test]
    fn refuses_flags_of_the_other_transport() {
        let exit = |args: &AttachArgs, acp, json| {
            check_flags(args, acp, json).map_err(|error| error.exit_code())
        };
        assert_eq!(exit(&args(None, true, false, false), false, true), Ok(()));
        assert_eq!(exit(&args(None, false, true, false), false, true), Ok(()));
        assert_eq!(exit(&args(None, false, false, true), true, false), Ok(()));
        assert_eq!(exit(&args(None, false, false, true), false, false), Err(2));
        assert_eq!(exit(&args(None, true, false, false), true, false), Err(2));
        assert_eq!(exit(&args(None, false, true, false), true, false), Err(2));
        assert_eq!(exit(&args(None, false, false, false), true, true), Err(2));
    }
}
