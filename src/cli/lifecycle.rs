//! `stop`, `restart`, `enable`, `disable`: one systemctl call each (enable
//! also writes its wants link), the command echoed, the status observed
//! afterwards reported. `stop --invocation` compares the unit's invocation
//! first, under the instance lock. `run` and
//! `start` share the start of an existing instance, the launch, the
//! template and mask checks, the stop intent and the linger warning.

use std::thread;
use std::time::{Duration, Instant};

use super::status::{self, Shown};
use crate::cli::report::{OutcomeSlug, StatusReport};
use crate::cli::{Context, NameArgs, StopArgs};
use crate::error::{Error, LINGER_REMEDY, OtherInvocation, StartFailure};
use crate::fsx::{self, Lock};
use crate::instance::{self, Instance};
use crate::name::{InstanceDir, InstanceName, RuntimeDir, TEMPLATE_FILE};
use crate::out::{self, Style};
use crate::record::Transport;
use crate::systemctl::{ActiveState, LoadState, UnitFacts, UnitResult};

/// How often a unit that is still moving is read again.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long a unit may stay activating or deactivating after `systemctl`
/// returned. `start` on a notify unit already waits for readiness, so a
/// unit still moving after this is restarting after a crash.
const SETTLE_BOUND: Duration = Duration::from_secs(10);
/// Journal lines a start failure carries.
const FAILURE_JOURNAL_LINES: u32 = 10;
/// Mode of the stop intent, like every file dshd writes for the runner.
const INTENT_MODE: u32 = 0o600;

/// The systemctl verb that launches the unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Launch {
    /// `systemctl start`.
    Start,
    /// `systemctl restart`.
    Restart,
}

/// `stop [--invocation ID] NAME`: stopped or already-stopped. With
/// `--invocation`, the instance lock is held from the comparison to the
/// end of the stop, so none of dshd's own starts comes between them, and
/// a unit on any other invocation than ID is left as it is.
pub(super) fn stop(ctx: &Context, args: &StopArgs) -> Result<(), Error> {
    let name = &args.name;
    let unit = name.unit();
    let _fence = match &args.invocation {
        Some(_) => lock_if_present(ctx, name)?,
        None => None,
    };
    let facts = ctx.systemctl.show(&unit)?;
    if let Some(expected) = &args.invocation {
        status::require(ctx, name, &facts)?;
        let dir = ctx.state_root.instance(name);
        require_invocation(name, &facts, || recorded_invocation(&dir), expected)?;
    }
    if matches!(
        facts.active_state,
        ActiveState::Inactive | ActiveState::Failed
    ) {
        let report = status::report_from(ctx, name, &facts, false)?;
        return status::print_outcome(ctx, Shown::new(OutcomeSlug::AlreadyStopped, report));
    }
    let intent = record_stop_intent(ctx, name)?;
    if let Err(error) = ctx.systemctl.stop(&unit) {
        withdraw_stop_intent(intent);
        return Err(match error {
            Error::Tool(failure) if failure.tool == "systemctl" => {
                let detail = if failure.stderr.is_empty() {
                    Error::Tool(failure).to_string()
                } else {
                    failure.stderr
                };
                Error::StopFailed {
                    name: name.to_string(),
                    detail,
                }
            }
            other => other,
        });
    }
    let report = status::report(ctx, name, false)?;
    let mut shown = Shown {
        before: vec![echoed(ctx, &["stop", &unit])],
        ..Shown::new(OutcomeSlug::Stopped, report)
    };
    if shown.status.enabled {
        let notice = |disable: &str| {
            format!(
                "{name} is enabled and starts again with the user manager or the next system switch; to keep it stopped: {disable}"
            )
        };
        let disable = format!("dshd disable {name}");
        shown
            .after
            .push(notice(&out::styled(Style::Command, &disable)));
        shown.notes.push(notice(&disable));
    }
    status::print_outcome(ctx, shown)
}

/// The instance lock, when the instance has a directory to hold it. A
/// unit the manager holds without one has no lock, and nothing of dshd's
/// can start it. A directory removed while waiting for the lock (`rm`
/// held it) is `NotFound`, as [`instance::lock_loaded`] reports it.
fn lock_if_present(ctx: &Context, name: &InstanceName) -> Result<Option<Lock>, Error> {
    let dir = ctx.state_root.instance(name);
    match fsx::kind(dir.path())? {
        Some(_) => match instance::lock(&dir) {
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(name.to_string()))
            }
            taken => taken.map(Some),
        },
        None => Ok(None),
    }
}

/// The invocation id of the run the runner last recorded, if any.
pub(super) fn recorded_invocation(dir: &InstanceDir) -> Result<Option<String>, Error> {
    Ok(instance::read_last_run(dir)?
        .map(|run| run.invocation_id)
        .filter(|id| !id.is_empty()))
}

/// `OtherInvocation` unless `expected` names the run `stop --invocation`
/// may stop or `rm --invocation` may remove: the unit's current
/// invocation while it is up; while it is not, its last one, which is
/// systemd's own while the manager still names it and else the one the
/// runner recorded (`recorded`, read only then). Ids compare without
/// regard to case.
pub(super) fn require_invocation(
    name: &InstanceName,
    facts: &UnitFacts,
    recorded: impl FnOnce() -> Result<Option<String>, Error>,
    expected: &str,
) -> Result<(), Error> {
    let up = status::unit_up(facts);
    let found = if !facts.invocation_id.is_empty() {
        Some(facts.invocation_id.clone())
    } else if up {
        None
    } else {
        recorded()?
    };
    if found
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case(expected))
    {
        return Ok(());
    }
    Err(Error::OtherInvocation(Box::new(OtherInvocation {
        name: name.to_string(),
        expected: expected.to_owned(),
        found,
        up,
    })))
}

/// `restart NAME`: reset-failed, restart, wait for active. A headless
/// instance is refused: a restart submits its task again.
pub(super) fn restart(ctx: &Context, args: &NameArgs) -> Result<(), Error> {
    require_template(ctx)?;
    let (lock, instance) = instance::lock_loaded(&ctx.state_root, &args.name)?;
    if instance.record.transport == Transport::None {
        return Err(Error::TaskAlreadyRan(args.name.to_string()));
    }
    refuse_masked(&ctx.systemctl.show(&args.name.unit())?, &args.name)?;
    record_stop_intent(ctx, &args.name)?;
    let echo = launch(ctx, &instance, Launch::Restart)?;
    let report = status::report(ctx, &args.name, false)?;
    drop(lock);
    let mut shown = Shown {
        before: vec![echo],
        ..Shown::new(OutcomeSlug::Restarted, report)
    };
    warn_without_linger(&mut shown, &stop_at_logout());
    status::print_outcome(ctx, shown)
}

/// `enable NAME`: the wants link and `daemon-reload` of
/// [`Systemctl::add_wants`](crate::systemctl::Systemctl::add_wants),
/// enabled or already-enabled.
/// A headless instance runs its task again at every start, so it is
/// refused; so is a masked unit.
pub(super) fn enable(ctx: &Context, args: &NameArgs) -> Result<(), Error> {
    let name = &args.name;
    let instance = instance::load_strict(&ctx.state_root, name)?;
    if instance.record.transport == Transport::None {
        return Err(Error::AutostartNotAllowed(name.to_string()));
    }
    let unit = name.unit();
    let facts = ctx.systemctl.show(&unit)?;
    refuse_masked(&facts, name)?;
    require_template(ctx)?;
    let (outcome, before) = if facts.enabled() {
        (OutcomeSlug::AlreadyEnabled, Vec::new())
    } else {
        let link = ctx.systemctl.add_wants(&unit)?;
        let linked = format!("ln -sfn {TEMPLATE_FILE} {}", link.display());
        (
            OutcomeSlug::Enabled,
            vec![
                out::styled(Style::Command, &linked),
                echoed(ctx, &["daemon-reload"]),
            ],
        )
    };
    let report = status::report(ctx, name, false)?;
    let mut shown = Shown {
        before,
        ..Shown::new(outcome, report)
    };
    warn_without_linger(
        &mut shown,
        &format!(
            "{name} starts at your first login, not at boot; to start it at boot, {LINGER_REMEDY}"
        ),
    );
    status::print_outcome(ctx, shown)
}

/// `disable NAME`: `systemctl --user disable`, disabled or already-disabled.
pub(super) fn disable(ctx: &Context, args: &NameArgs) -> Result<(), Error> {
    let name = &args.name;
    let unit = name.unit();
    let facts = ctx.systemctl.show(&unit)?;
    status::require(ctx, name, &facts)?;
    let (outcome, before) = if facts.enabled() {
        ctx.systemctl.disable(&unit)?;
        (
            OutcomeSlug::Disabled,
            vec![echoed(ctx, &["disable", &unit])],
        )
    } else {
        (OutcomeSlug::AlreadyDisabled, Vec::new())
    };
    let report = status::report(ctx, name, false)?;
    status::print_outcome(
        ctx,
        Shown {
            before,
            ..Shown::new(outcome, report)
        },
    )
}

/// Starts an existing instance the caller holds the lock of, or reports
/// that it runs already. Only an active unit counts as running. A masked
/// unit is refused, and so is a headless instance whose task already ran
/// unless `rerun` says to run it again. `created` names the outcome of a
/// start right after creation.
pub(super) fn bring_up(
    ctx: &Context,
    instance: &Instance,
    rerun: bool,
    created: bool,
) -> Result<Shown, Error> {
    let name = &instance.name;
    let facts = ctx.systemctl.show(&name.unit())?;
    let mut before = Vec::new();
    let outcome = if facts.active_state == ActiveState::Active {
        OutcomeSlug::AlreadyRunning
    } else {
        refuse_masked(&facts, name)?;
        if created {
            before.push(format!("Created dshd instance {name}."));
        }
        if instance.record.transport == Transport::None
            && instance::read_last_run(&instance.dir)?.is_some()
        {
            if !rerun {
                return Err(Error::TaskAlreadyRan(name.to_string()));
            }
            before.push("re-running the recorded task".to_owned());
        }
        before.push(launch(ctx, instance, Launch::Start)?);
        if created {
            OutcomeSlug::Created
        } else {
            OutcomeSlug::Started
        }
    };
    let report = status::report(ctx, name, false)?;
    let mut shown = Shown {
        before,
        after: next_step(&report),
        ..Shown::new(outcome, report)
    };
    warn_without_linger(&mut shown, &stop_at_logout());
    Ok(shown)
}

/// The command that uses an instance that runs: attach to a web or acp
/// instance, follow a headless one.
fn next_step(report: &StatusReport) -> Vec<String> {
    let (lead, command) = match report.transport {
        Some(Transport::Web | Transport::Acp) => {
            ("Attach with", format!("dshd attach {}", report.name))
        }
        Some(Transport::None) => ("Follow with", format!("dshd read --follow {}", report.name)),
        None => return Vec::new(),
    };
    vec![format!("{lead}: {}", out::styled(Style::Command, &command))]
}

/// `TemplateMissing` unless the user manager has the unit template.
pub(super) fn require_template(ctx: &Context) -> Result<(), Error> {
    if ctx.systemctl.template_installed()? {
        Ok(())
    } else {
        Err(Error::TemplateMissing)
    }
}

/// `UnitMasked` for a masked unit: systemd refuses to start it, and only
/// its user unmasks it.
pub(super) fn refuse_masked(facts: &UnitFacts, name: &InstanceName) -> Result<(), Error> {
    if facts.load_state == LoadState::Masked {
        Err(Error::UnitMasked(name.unit()))
    } else {
        Ok(())
    }
}

/// Leaves the stop intent the runner's post-stop step reads, so the run's
/// end is recorded as dshd's stop. Only a run has a runtime directory;
/// without one there is nothing to stop and nothing is written. A write
/// that fails is an error, and the caller stops nothing. Returns the file
/// written.
pub(super) fn record_stop_intent(
    ctx: &Context,
    name: &InstanceName,
) -> Result<Option<RuntimeDir>, Error> {
    let runtime = RuntimeDir::under(&ctx.runtime_root, name);
    if fsx::kind(runtime.path())?.is_none() {
        return Ok(None);
    }
    fsx::write_atomic(&runtime.stop_intent(), b"", INTENT_MODE)?;
    Ok(Some(runtime))
}

/// Removes a stop intent whose stop did not happen, so a later end of the
/// run is not taken for dshd's.
fn withdraw_stop_intent(intent: Option<RuntimeDir>) {
    if let Some(runtime) = intent {
        // The stop failure is what gets reported; an intent that cannot be
        // removed only labels this run's end as dshd's, and dies with it.
        drop(fsx::remove_tree(&runtime.stop_intent()));
    }
}

/// Clears a failed state, starts or restarts the unit and waits for it to
/// settle. Returns the echoed command. A unit that did not come up is
/// `StartFailed`, with the last lines of its journal. A headless unit that
/// started and has ended already came up: its task ran.
pub(super) fn launch(ctx: &Context, instance: &Instance, how: Launch) -> Result<String, Error> {
    let name = &instance.name;
    let unit = name.unit();
    ctx.systemctl.reset_failed(&unit)?;
    let (verb, acted) = match how {
        Launch::Start => ("start", ctx.systemctl.start(&unit)),
        Launch::Restart => ("restart", ctx.systemctl.restart(&unit)),
    };
    match acted {
        Ok(()) => {}
        // systemctl exits non-zero when the unit failed to come up; the
        // unit's own state then says how.
        Err(Error::Tool(failure)) if failure.tool == "systemctl" => {
            let facts = ctx.systemctl.show(&unit)?;
            return Err(if did_not_come_up(&facts, false) {
                start_failure(ctx, name, &facts)
            } else {
                Error::Tool(failure)
            });
        }
        Err(other) => return Err(other),
    }
    let facts = settle(ctx, &unit)?;
    if did_not_come_up(&facts, instance.record.transport == Transport::None) {
        return Err(start_failure(ctx, name, &facts));
    }
    Ok(echoed(ctx, &[verb, &unit]))
}

/// The echoed systemctl command line, styled as a command.
pub(super) fn echoed(ctx: &Context, args: &[&str]) -> String {
    out::styled(Style::Command, &ctx.systemctl.echo(args))
}

/// Adds the linger warning when linger is off: `consequence` says what
/// that means for this verb.
pub(super) fn warn_without_linger(shown: &mut Shown, consequence: &str) {
    if !shown.status.linger {
        shown
            .warnings
            .push(format!("linger is off, so {consequence}"));
    }
}

/// The consequence of linger being off after a start.
fn stop_at_logout() -> String {
    format!(
        "instances stop when your last login session ends; to keep them running, {LINGER_REMEDY}"
    )
}

/// Reads the unit until it stops moving, at most [`SETTLE_BOUND`].
pub(super) fn settle(ctx: &Context, unit: &str) -> Result<UnitFacts, Error> {
    let deadline = Instant::now() + SETTLE_BOUND;
    let mut facts = ctx.systemctl.show(unit)?;
    while matches!(
        facts.active_state,
        ActiveState::Activating | ActiveState::Deactivating
    ) && Instant::now() < deadline
    {
        thread::sleep(POLL_INTERVAL);
        facts = ctx.systemctl.show(unit)?;
    }
    Ok(facts)
}

/// Failed, still moving, or inactive after an unclean end. A unit that
/// ran and ended cleanly came up; so did a headless unit (`task_ran`) that
/// systemd started and that has ended in any way, since its end is the
/// task's result, which `run` and `wait` report.
fn did_not_come_up(facts: &UnitFacts, task_ran: bool) -> bool {
    match facts.active_state {
        ActiveState::Failed | ActiveState::Inactive if task_ran => false,
        ActiveState::Activating | ActiveState::Deactivating | ActiveState::Failed => true,
        ActiveState::Inactive => facts.result != UnitResult::Success || facts.exec_main_status != 0,
        ActiveState::Active | ActiveState::Unknown(_) => false,
    }
}

fn start_failure(ctx: &Context, name: &InstanceName, facts: &UnitFacts) -> Error {
    let journal_tail =
        match ctx
            .systemctl
            .journal_tail(&ctx.journalctl, &name.unit(), FAILURE_JOURNAL_LINES)
        {
            Ok(lines) => lines,
            // The tail only illustrates the failure, which is reported either way.
            Err(error) => vec![format!("(the journal could not be read: {error})")],
        };
    Error::StartFailed(Box::new(StartFailure {
        name: name.to_string(),
        active_state: status::active_word(&facts.active_state),
        result: status::result_word(&facts.result),
        journal_tail,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systemctl::parse_show;

    fn facts(fixture: &str) -> UnitFacts {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/show")
            .join(fixture);
        parse_show(&std::fs::read_to_string(path).unwrap())
            .unwrap()
            .pop()
            .unwrap()
    }

    // catches: a crash loop or an unclean exit reported as a successful start, or a headless task's
    // own failure reported as a failed start instead of as its exit
    #[test]
    fn crash_loop_is_a_failed_start() {
        assert!(!did_not_come_up(&facts("running.txt"), false));
        assert!(
            !did_not_come_up(&facts("inactive.txt"), false),
            "a clean run that ended came up"
        );
        assert!(did_not_come_up(&facts("failed-start-limit.txt"), false));
        assert!(did_not_come_up(&facts("auto-restart.txt"), false));
        assert!(
            did_not_come_up(&facts("auto-restart.txt"), true),
            "a restart loop is no task result"
        );
        let unclean = UnitFacts {
            exec_main_status: 78,
            ..facts("inactive.txt")
        };
        assert!(did_not_come_up(&unclean, false));
        assert!(
            !did_not_come_up(&unclean, true),
            "a headless task that exited 78 ran"
        );
        assert!(!did_not_come_up(&facts("failed-start-limit.txt"), true));
    }

    // catches: a compare-and-stop that stops a newer run of the same name, refuses the run it names,
    // or trusts the recorded run over the invocation systemd still names
    #[test]
    fn stop_invocation_names_the_held_or_last_run() {
        let name = InstanceName::parse("a").unwrap();
        let (current, older) = (
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        let unread = || -> Result<Option<String>, Error> {
            panic!("the record is read only when systemd names no run")
        };
        let recorded = |id: &'static str| move || Ok(Some(id.to_owned()));
        let refused = |result: Result<(), Error>| match result {
            Err(Error::OtherInvocation(other)) => *other,
            other => panic!("not refused: {other:?}"),
        };
        let running = facts("running.txt");
        assert!(require_invocation(&name, &running, unread, current).is_ok());
        let upper = current.to_ascii_uppercase();
        assert!(require_invocation(&name, &running, unread, &upper).is_ok());
        assert_eq!(
            refused(require_invocation(&name, &running, unread, older)),
            OtherInvocation {
                name: "a".into(),
                expected: older.into(),
                found: Some(current.into()),
                up: true,
            }
        );
        let stopped = facts("inactive.txt");
        assert!(require_invocation(&name, &stopped, recorded(older), older).is_ok());
        let other = refused(require_invocation(
            &name,
            &stopped,
            recorded(older),
            current,
        ));
        assert_eq!((other.found.as_deref(), other.up), (Some(older), false));
        let never_ran = refused(require_invocation(&name, &stopped, || Ok(None), current));
        assert_eq!((never_ran.found, never_ran.up), (None, false));
        let remembered = UnitFacts {
            invocation_id: current.into(),
            ..facts("inactive.txt")
        };
        assert!(require_invocation(&name, &remembered, unread, current).is_ok());
        assert!(require_invocation(&name, &remembered, unread, older).is_err());
        let between = refused(require_invocation(
            &name,
            &facts("auto-restart.txt"),
            unread,
            current,
        ));
        assert_eq!(
            (between.found, between.up),
            (None, true),
            "a unit between runs that names no invocation holds none"
        );
    }
}
