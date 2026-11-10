//! Poststop completion: the observed invocation end, release-root collapse and per-run cleanup.

use std::io;
use std::path::Path;

use super::{
    CHILD_END_FILE, EXIT_CODE, EXIT_CONFIG, EXIT_STATUS, INVOCATION_ID, PRIVATE_MODE, Place,
    RunnerEnd, SERVICE_RESULT, log_line, unit_value,
};
use crate::error::Error;
use crate::record::{LastRun, OsText, RunEnd};
use crate::{fsx, instance, release, systemctl};

/// Records the run, then removes the per-run files whatever happened;
/// the status `poststop` exits with.
pub(super) fn record_and_clear(place: &Place) -> u8 {
    let status = match record_last_run(place) {
        Ok(()) => match release::collapse_booted(&place.dir) {
            Ok(_) => 0,
            Err(error) => {
                log_line(&format!("cannot release the ended closure: {error}"));
                EXIT_CONFIG
            }
        },
        Err(error) => {
            log_line(&format!("cannot record the run: {error}"));
            EXIT_CONFIG
        }
    };
    let per_run = [
        place.runtime.web_url(),
        place.runtime.acp_socket(),
        place.runtime_file(CHILD_END_FILE),
        place.runtime.stop_intent(),
    ];
    for file in per_run {
        if let Err(error) = remove_if_present(&file) {
            log_line(&error.to_string());
        }
    }
    status
}

/// `last-run.json` for the run that just ended: the start half the runner
/// wrote for this invocation, completed with its end. A runner that never
/// got that far left none, and the end time stands in for the start.
fn record_last_run(place: &Place) -> Result<(), Error> {
    let ended_at = fsx::unix_now();
    let invocation_id = unit_value(INVOCATION_ID);
    let path = place.dir.last_run_json();
    let mut run = match start_half(&path, &invocation_id) {
        Some(start) => start,
        None => LastRun {
            id: instance::load(&place.root, &place.name)?.record.id,
            release_path: fsx::read_link(&place.dir.booted())?
                .map_or_else(|| OsText::from(""), OsText::new),
            boot_id: systemctl::boot_id()?,
            started_at: ended_at,
            invocation_id,
            end: None,
            unrecognized: serde_json::Map::new(),
        },
    };
    let seen: Option<RunnerEnd> = fsx::read_json(&place.runtime_file(CHILD_END_FILE))?;
    run.end = Some(RunEnd {
        ended_at,
        service_result: unit_value(SERVICE_RESULT),
        exit_code: unit_value(EXIT_CODE),
        exit_status: unit_value(EXIT_STATUS),
        child: seen.as_ref().map(|seen| seen.child),
        stop_intent: fsx::kind(&place.runtime.stop_intent())?.is_some(),
        stop_signal: seen.and_then(|seen| seen.stop_signal),
        unrecognized: serde_json::Map::new(),
    });
    fsx::write_json_atomic(&path, &run, PRIVATE_MODE)
}

/// The start half `last-run.json` holds for this invocation, if it does. An
/// unreadable file is reported and replaced.
fn start_half(path: &Path, invocation_id: &str) -> Option<LastRun> {
    match fsx::read_json::<LastRun>(path) {
        Ok(start) => {
            start.filter(|start| !invocation_id.is_empty() && start.invocation_id == invocation_id)
        }
        Err(error) => {
            log_line(&format!(
                "the start of the run is unreadable, so its end time stands in: {error}"
            ));
            None
        }
    }
}

/// Removes one per-run file; one that is already gone is fine.
fn remove_if_present(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => {
            Err(Error::io("remove", path, source))
        }
        Ok(()) | Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: a poststop that fails because a per-run file the run never wrote is absent (P2.5)
    #[test]
    fn removing_a_missing_file_is_fine() {
        let missing =
            std::env::temp_dir().join(format!("dshd-runner-missing-{}", std::process::id()));
        assert!(remove_if_present(&missing).is_ok());
    }
}
