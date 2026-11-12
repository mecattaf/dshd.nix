//! `dshd get NAME`: gathers unit facts, record, links, last run, web URL
//! and (for acp) the relay probe into one `StatusReport`. Every other verb
//! builds the report it prints after acting here too, and prints its
//! outcome through [`print_outcome`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;

use super::render;
use crate::cli::report::{
    LastRunReport, Outcome, OutcomeSlug, ProfileReport, ReleaseReport, RunExit, StateSlug,
    StatusReport, WebReport,
};
use crate::cli::{Context, NameArgs};
use crate::error::Error;
use crate::fsx;
use crate::instance::{self, Instance};
use crate::launch;
use crate::name::{self, InstanceName, RuntimeDir};
use crate::out;
use crate::record::{LastRun, OsText, Transport};
use crate::relay::{self, protocol::ProbeLine};
use crate::release::{self, ReleaseState};
use crate::runner::web;
use crate::systemctl::{self, ActiveState, LoadState, Task, UnitFacts, UnitResult};
use crate::tool::Tool;

/// A report and what only the human page adds to it.
#[derive(Debug)]
pub(super) struct Gathered {
    /// The report.
    pub(super) report: StatusReport,
    /// Why the relay probe gave no answer, when it ran.
    pub(super) probe_failure: Option<String>,
    /// What could not be read on disk, or contradicts itself.
    pub(super) problems: Vec<String>,
}

/// A mutating verb's result, and what its text form adds around the
/// status line.
#[derive(Debug)]
pub(super) struct Shown {
    /// The outcome.
    pub(super) outcome: OutcomeSlug,
    /// The status observed after the action.
    pub(super) status: StatusReport,
    /// Text form: lines before the status line.
    pub(super) before: Vec<String>,
    /// Text form: lines after it.
    pub(super) after: Vec<String>,
    /// Both forms: on stderr as `dshd: warning: ...` in text, in
    /// `warnings` in JSON.
    pub(super) warnings: Vec<String>,
    /// JSON only: what `before` and `after` say that a reader must know.
    pub(super) notes: Vec<String>,
    /// For `run` and `wait`: how the followed run ended.
    pub(super) exit: Option<RunExit>,
}

impl Shown {
    /// An outcome with nothing around its status line.
    pub(super) fn new(outcome: OutcomeSlug, status: StatusReport) -> Self {
        Self {
            outcome,
            status,
            before: Vec::new(),
            after: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            exit: None,
        }
    }
}

/// Problems met while gathering. A fault makes the on-disk state unsound,
/// so it reads `inconsistent` unless the unit is up; a note is only shown.
#[derive(Debug, Default)]
struct Findings {
    problems: Vec<String>,
    unsound: bool,
}

impl Findings {
    fn fault(&mut self, problem: String) {
        self.unsound = true;
        self.problems.push(problem);
    }

    fn note(&mut self, problem: String) {
        self.problems.push(problem);
    }
}

/// Runs the verb. The human page adds the probe failure, the problems
/// found on disk and, last, whether linger is on.
pub(super) fn run(ctx: &Context, args: &NameArgs) -> Result<(), Error> {
    let facts = ctx.systemctl.show(&args.name.unit())?;
    let gathered = gather(ctx, &args.name, &facts, &systemctl::boot_id()?, true, false)?;
    if ctx.json {
        return out::json(&gathered.report);
    }
    let mut lines = vec![render::status_page(&gathered.report)];
    if let Some(reason) = &gathered.probe_failure {
        lines.push(format!("  acp: unknown ({reason})"));
    }
    lines.extend(
        gathered
            .problems
            .iter()
            .map(|problem| format!("  problem: {problem}")),
    );
    lines.push(render::linger_line(gathered.report.linger));
    out::line(&lines.join("\n"))
}

/// Builds the report every other verb prints after acting. `probe` says
/// whether to run the relay probe for an acp instance.
pub(super) fn report(
    ctx: &Context,
    name: &InstanceName,
    probe: bool,
) -> Result<StatusReport, Error> {
    let facts = ctx.systemctl.show(&name.unit())?;
    report_from(ctx, name, &facts, probe)
}

/// [`report`] from unit facts the caller already read.
pub(super) fn report_from(
    ctx: &Context,
    name: &InstanceName,
    facts: &UnitFacts,
    probe: bool,
) -> Result<StatusReport, Error> {
    Ok(gather(ctx, name, facts, &systemctl::boot_id()?, probe, false)?.report)
}

/// `NotFound` unless the instance has a directory or the manager has
/// held its unit since it loaded.
pub(super) fn require(ctx: &Context, name: &InstanceName, facts: &UnitFacts) -> Result<(), Error> {
    if known_to_manager(facts) || fsx::kind(ctx.state_root.instance(name).path())?.is_some() {
        Ok(())
    } else {
        Err(Error::NotFound(name.to_string()))
    }
}

/// Gathers one instance. `listed` says the manager listed the unit, so an
/// instance without a directory is orphaned rather than unknown.
pub(super) fn gather(
    ctx: &Context,
    name: &InstanceName,
    facts: &UnitFacts,
    boot: &str,
    probe: bool,
    listed: bool,
) -> Result<Gathered, Error> {
    let mut findings = Findings::default();
    let instance = load_record(ctx, name, facts, listed, &mut findings)?;
    let last_run = instance
        .as_ref()
        .and_then(|instance| last_run_of(instance, &mut findings));
    let release = instance
        .as_ref()
        .map(|instance| release_of(instance, &mut findings));
    let record = instance.as_ref().map(|instance| &instance.record);
    let mut state = systemctl::derive_state(Some(facts), record, last_run.as_ref(), boot);
    if findings.unsound && !unit_up(facts) {
        state = StateSlug::Inconsistent;
    }
    let task = match (record.map(|record| record.transport), &last_run) {
        (Some(Transport::None), None) => Task::NotRun,
        (Some(Transport::None), Some(run)) => Task::Ended(systemctl::ending(run)),
        _ => Task::NotHeadless,
    };
    let remedy = systemctl::remedy(state, name, unit_up(facts), task).or_else(|| {
        release
            .as_ref()
            .and_then(|(_, release_state)| release_remedy(*release_state, state, name))
    });
    let linger = systemctl::linger_enabled(&ctx.user);
    let mut report = bare_report(name, facts, state, facts.enabled(), linger);
    report.remedy = remedy;
    report.release = release.map(|(release, _)| release);
    report.last_run = last_run.map(|run| last_run_report(run, boot, unit_up(facts)));
    let mut probe_failure = None;
    if let Some(instance) = &instance {
        fill_recipe(&mut report, instance);
        let running = facts.active_state == ActiveState::Active;
        match instance.record.transport {
            Transport::Web => report.web = Some(web_report(ctx, instance, running, &mut findings)),
            Transport::Acp if probe && running => match probe_relay(ctx, instance) {
                Ok(line) => report.acp = Some(line),
                Err(error) => probe_failure = Some(error.to_string()),
            },
            Transport::Acp | Transport::None => {}
        }
    }
    Ok(Gathered {
        report,
        probe_failure,
        problems: findings.problems,
    })
}

/// Prints a mutating verb's result: the JSON outcome, or the `before`
/// lines, the status line and the `after` lines on stdout and the
/// warnings on stderr.
pub(super) fn print_outcome(ctx: &Context, shown: Shown) -> Result<(), Error> {
    let Shown {
        outcome,
        status,
        before,
        after,
        mut warnings,
        notes,
        exit,
    } = shown;
    if ctx.json {
        warnings.extend(notes);
        return out::json(&Outcome {
            outcome,
            warnings,
            exit,
            status,
        });
    }
    let mut lines = before;
    lines.push(render::line(&render::entry_of(&status)));
    lines.extend(after);
    out::line(&lines.join("\n"))?;
    for warning in &warnings {
        out::error_line(&format!("dshd: warning: {warning}"));
    }
    Ok(())
}

/// systemd's `ActiveState` word.
pub(super) fn active_word(state: &ActiveState) -> String {
    match state {
        ActiveState::Active => "active",
        ActiveState::Activating => "activating",
        ActiveState::Deactivating => "deactivating",
        ActiveState::Inactive => "inactive",
        ActiveState::Failed => "failed",
        ActiveState::Unknown(word) => word.as_str(),
    }
    .to_owned()
}

/// systemd's `Result` word.
pub(super) fn result_word(result: &UnitResult) -> String {
    match result {
        UnitResult::Success => "success",
        UnitResult::ExitCode => "exit-code",
        UnitResult::Signal => "signal",
        UnitResult::Timeout => "timeout",
        UnitResult::CoreDump => "core-dump",
        UnitResult::StartLimitHit => "start-limit-hit",
        UnitResult::Unknown(word) => word.as_str(),
    }
    .to_owned()
}

/// The manager knows the unit: it is loaded and has been active at some
/// point. Any name of an installed template loads, so loading alone says
/// nothing.
fn known_to_manager(facts: &UnitFacts) -> bool {
    facts.load_state != LoadState::NotFound
        && (facts.active_state != ActiveState::Inactive || facts.inactive_enter.is_some())
}

/// The unit holds or is moving a process, so its state stands even over an
/// unsound record.
pub(super) fn unit_up(facts: &UnitFacts) -> bool {
    matches!(
        facts.active_state,
        ActiveState::Active | ActiveState::Activating | ActiveState::Deactivating
    )
}

/// The instance as recorded. A record dshd would refuse to act on is a
/// fault but still shown; an unreadable one is a fault and shows nothing.
fn load_record(
    ctx: &Context,
    name: &InstanceName,
    facts: &UnitFacts,
    listed: bool,
    findings: &mut Findings,
) -> Result<Option<Instance>, Error> {
    match instance::load_strict(&ctx.state_root, name) {
        Ok(instance) => Ok(Some(instance)),
        Err(Error::NotFound(_)) if listed || known_to_manager(facts) => Ok(None),
        Err(Error::InconsistentState { reason, .. }) => {
            findings.fault(reason);
            match instance::load(&ctx.state_root, name) {
                Ok(instance) => Ok(Some(instance)),
                Err(Error::InconsistentState { .. }) => Ok(None),
                Err(other) => Err(other),
            }
        }
        Err(other) => Err(other),
    }
}

fn last_run_of(instance: &Instance, findings: &mut Findings) -> Option<LastRun> {
    match instance::read_last_run(&instance.dir) {
        Ok(run) => run,
        Err(error) => {
            findings.fault(error.to_string());
            None
        }
    }
}

/// The release block. A `current` link that is missing or dangles is a
/// fault; a registry that cannot be read is a note, since it is not the
/// instance's own state.
fn release_of(instance: &Instance, findings: &mut Findings) -> (ReleaseReport, ReleaseState) {
    let recorded = &instance.record.release;
    let (current, booted) = match release::links(&instance.dir) {
        Ok((None, booted)) => {
            findings.fault(format!(
                "{} is missing or dangles",
                instance.dir.current().display()
            ));
            (None, booted)
        }
        Ok(links) => links,
        Err(error) => {
            findings.fault(error.to_string());
            (None, None)
        }
    };
    let registry = match release::registry_target(Path::new(name::RELEASES_DIR), recorded) {
        Ok(target) => target,
        Err(error) => {
            findings.note(error.to_string());
            None
        }
    };
    let state = release::release_state(current.as_deref(), booted.as_deref(), registry.as_deref());
    let label = match current.as_deref().or(booted.as_deref()) {
        Some(path) => release::version_label(path),
        None => recorded.clone(),
    };
    let report = ReleaseReport {
        name: recorded.clone(),
        current: current.as_deref().map(text),
        booted: booted.as_deref().map(text),
        registry: registry.as_deref().map(text),
        state: state.slug(),
        label,
    };
    (report, state)
}

/// The command that brings a running or stopped instance onto its
/// release, when the unit's own state needs none.
fn release_remedy(release: ReleaseState, state: StateSlug, name: &InstanceName) -> Option<String> {
    let running = state == StateSlug::Running;
    match release {
        ReleaseState::Outdated if running => Some(format!("dshd update -R {name}")),
        ReleaseState::Outdated => Some(format!("dshd update {name}")),
        ReleaseState::Stale if running => Some(format!("dshd restart {name}")),
        ReleaseState::Stale | ReleaseState::Current | ReleaseState::Missing => None,
    }
}

/// The report from the unit alone; the recipe fields stay empty.
fn bare_report(
    name: &InstanceName,
    facts: &UnitFacts,
    state: StateSlug,
    enabled: bool,
    linger: bool,
) -> StatusReport {
    StatusReport {
        name: name.to_string(),
        id: None,
        unit: name.unit(),
        state,
        active_state: active_word(&facts.active_state),
        sub_state: facts.sub_state.clone(),
        result: result_word(&facts.result),
        main_pid: (facts.main_pid != 0).then_some(facts.main_pid),
        restarts: facts.restarts,
        resources: facts.resources,
        invocation_id: (!facts.invocation_id.is_empty()).then(|| facts.invocation_id.clone()),
        since: since(facts),
        enabled,
        linger,
        transport: None,
        cwd: None,
        args: Vec::new(),
        environment_names: Vec::new(),
        labels: BTreeMap::new(),
        release: None,
        profile: None,
        web: None,
        acp: None,
        last_run: None,
        remedy: None,
    }
}

/// When the unit entered the state it is in.
fn since(facts: &UnitFacts) -> Option<u64> {
    match facts.active_state {
        ActiveState::Active | ActiveState::Deactivating => facts.active_enter,
        ActiveState::Activating
        | ActiveState::Inactive
        | ActiveState::Failed
        | ActiveState::Unknown(_) => facts.inactive_enter,
    }
}

/// The recorded recipe: identity, transport, cwd, argv, environment names,
/// labels and the profile block.
fn fill_recipe(report: &mut StatusReport, instance: &Instance) {
    let record = &instance.record;
    report.id = Some(record.id.clone());
    report.transport = Some(record.transport);
    report.cwd = Some(record.cwd.display());
    report.args = record.args.iter().map(OsText::display).collect();
    report.environment_names = record
        .environment
        .iter()
        .map(|var| var.name.display())
        .collect();
    report.labels = record.labels.clone();
    report.profile = Some(ProfileReport {
        name: record.profile.name.clone(),
        dsh_home: record.profile.dsh_home.display(),
        patches: recorded_patches(&record.args),
    });
}

/// The `--patch` overlays of a recorded argv, by the launcher grammar that
/// accepted it at creation; none for an argv it no longer accepts.
fn recorded_patches(args: &[OsText]) -> Vec<String> {
    let tokens: Vec<OsString> = args.iter().map(|arg| arg.as_os_str().to_owned()).collect();
    launch::selection(&tokens)
        .map(|selection| {
            selection
                .patches
                .into_iter()
                .map(|patch| OsText::new(patch).display())
                .collect()
        })
        .unwrap_or_default()
}

/// The pinned port, and the announced address with its secret redacted
/// while the unit runs.
fn web_report(
    ctx: &Context,
    instance: &Instance,
    running: bool,
    findings: &mut Findings,
) -> WebReport {
    let port = match instance::read_web_port(&instance.dir) {
        Ok(port) => port,
        Err(error) => {
            findings.note(error.to_string());
            None
        }
    };
    if !running {
        return WebReport { port, url: None };
    }
    let url = match fsx::read_text(&RuntimeDir::under(&ctx.runtime_root, &instance.name).web_url())
    {
        Ok(announced) => announced.map(|announced| web::redact_tokens(announced.trim())),
        Err(error) => {
            findings.note(error.to_string());
            None
        }
    };
    WebReport { port, url }
}

/// Runs `booted/bin/dshd-run probe NAME` and waits at most the probe
/// timeout for its one JSON line.
fn probe_relay(ctx: &Context, instance: &Instance) -> Result<ProbeLine, Error> {
    let runner = Tool::at("dshd-run", name::booted_runner(&instance.dir));
    let args = [
        OsString::from("probe"),
        OsString::from(instance.name.as_str()),
    ];
    let mut command = runner.command();
    command
        .args(&args)
        .env("XDG_RUNTIME_DIR", &ctx.runtime_root);
    let (sender, receiver) = mpsc::channel();
    let probing =
        |kind: io::ErrorKind| Error::io("probe", runner.program.clone(), io::Error::from(kind));
    let spawned = thread::Builder::new()
        .name("probe".to_owned())
        .spawn(move || {
            // After a timeout nobody waits for the answer, and it is dropped.
            drop(sender.send(command.output()));
        });
    if let Err(error) = spawned {
        return Err(Error::io("start a thread for", "the relay probe", error));
    }
    let output = match receiver.recv_timeout(relay::PROBE_TIMEOUT) {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(runner.spawn_error(error)),
        Err(RecvTimeoutError::Timeout) => return Err(probing(io::ErrorKind::TimedOut)),
        Err(RecvTimeoutError::Disconnected) => return Err(probing(io::ErrorKind::BrokenPipe)),
    };
    let captured = runner.check(&args, output)?;
    serde_json::from_slice(&captured.stdout)
        .map_err(|error| Error::io("parse", "the relay probe answer", error.into()))
}

/// The last run block. A run without a recorded end while the unit is up
/// is the run in progress, so it has no ending yet.
fn last_run_report(run: LastRun, boot: &str, up: bool) -> LastRunReport {
    let ending = (run.end.is_some() || !up).then(|| systemctl::ending(&run));
    let this_boot = run.boot_id == boot;
    let LastRun {
        started_at,
        invocation_id,
        end,
        ..
    } = run;
    let mut report = LastRunReport {
        started_at,
        invocation_id,
        ending,
        ended_at: None,
        service_result: None,
        exit_code: None,
        exit_status: None,
        child: None,
        this_boot,
    };
    if let Some(end) = end {
        report.ended_at = Some(end.ended_at);
        report.service_result = Some(end.service_result);
        report.exit_code = Some(end.exit_code);
        report.exit_status = Some(end.exit_status);
        report.child = end.child;
    }
    report
}

/// A path for a report field, lossless for any bytes.
fn text(path: &Path) -> String {
    OsText::new(path.as_os_str()).display()
}

#[cfg(test)]
#[path = "status/unit_tests.rs"]
mod tests;
