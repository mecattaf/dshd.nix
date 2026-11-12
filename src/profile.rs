//! Profiles are DSH's own. An instance runs the live profile of its DSH
//! home, and dshd writes nothing there: it reads the profile's manifest to
//! learn the transport its bundles select, and leaves a shipped profile
//! that does not exist yet for dsh itself to materialise.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::{Error, UnheldApp};
use crate::fsx;
use crate::record::Transport;
use crate::tool::Tool;

/// The profiles directory of a DSH home.
const PROFILES_DIR: &str = "profiles";
/// The profile manifest inside a profile directory.
const MANIFEST: &str = "package.json";
/// The dsh flag that composes a profile's configuration and prints it
/// without booting the profile, materialising a shipped profile first.
const DUMP_DEFAULT_CONFIG: &str = "--dump-default-config";

/// The bundle names that decide the transport, compared with the text after
/// the last `/`, so a scope (`@deepseek-ai/`) does not matter. A headless
/// profile runs its task with no transport.
const HEADLESS_BUNDLE: &str = "dsh-headless";
/// The web app bundle.
const WEB_BUNDLE: &str = "dsh-web-app";
/// The ACP app bundle.
const ACP_BUNDLE: &str = "dsh-acp-app";
/// The SDK host bundles: refused, an SDK host serves the client that
/// starts it on stdio, which a unit cannot be.
const SDK_BUNDLES: [&str; 2] = [SDK_APP_BUNDLE, SDK_MINIMAL_BUNDLE];
/// The SDK app bundle.
const SDK_APP_BUNDLE: &str = "dsh-sdk-app";
/// The minimal SDK bundle.
const SDK_MINIMAL_BUNDLE: &str = "dsh-sdk-minimal";
/// Every app bundle dshd knows, in the order that decides the transport.
const APP_BUNDLES: [&str; 5] = [
    HEADLESS_BUNDLE,
    WEB_BUNDLE,
    ACP_BUNDLE,
    SDK_APP_BUNDLE,
    SDK_MINIMAL_BUNDLE,
];

/// What a profile's live manifest selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The transport its bundles select.
    pub transport: Transport,
    /// Every app bundle it lists, in the order that decides the transport.
    /// DSH composes every bundle a profile lists, so more than one means
    /// several apps mounted in one process.
    pub apps: Vec<&'static str>,
}

/// The part of `package.json` that names the profile's bundles. Every other
/// key is DSH's and ignored.
#[derive(Debug, Default, Deserialize)]
struct Manifest {
    #[serde(default)]
    dsh: ManifestDsh,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestDsh {
    #[serde(default)]
    profile: ManifestProfile,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestProfile {
    #[serde(default)]
    bundles: Vec<String>,
}

/// The transport `profile` in `dsh_home` selects now, from its live
/// manifest. A profile without a manifest is materialised by dsh first, as
/// its own first boot of the profile would: `command` is `dsh` set up with
/// the environment and directory the instance runs it with, and runs with
/// `--profile <profile> --dump-default-config`. dsh creates a profile it
/// ships and refuses any other name; its refusal is the error.
pub fn transport(
    dsh: &Tool,
    mut command: Command,
    dsh_home: &Path,
    profile: &str,
) -> Result<Detected, Error> {
    let manifest = dsh_home.join(PROFILES_DIR).join(profile).join(MANIFEST);
    if fsx::kind(&manifest)?.is_none() {
        let args = [
            OsString::from("--profile"),
            OsString::from(profile),
            OsString::from(DUMP_DEFAULT_CONFIG),
        ];
        let output = command
            .args(&args)
            .stdout(Stdio::null())
            .output()
            .map_err(|e| dsh.spawn_error(e))?;
        dsh.check(&args, output)?;
    }
    detect_transport(&manifest, profile)
}

/// Maps the manifest's `dsh.profile.bundles`, in this order: `dsh-headless`
/// → `None`, `dsh-web-app` → `Web`, `dsh-acp-app` → `Acp`. An SDK bundle,
/// or a profile with none of these (no manifest, no bundles, a terminal
/// app), is `UnsupportedProfile`.
fn detect_transport(manifest: &Path, profile: &str) -> Result<Detected, Error> {
    let bundles = fsx::read_json::<Manifest>(manifest)?
        .unwrap_or_default()
        .dsh
        .profile
        .bundles;
    let has = |wanted: &str| {
        bundles
            .iter()
            .any(|bundle| bundle.rsplit('/').next() == Some(wanted))
    };
    let refuse = |app| {
        Err(Error::UnsupportedProfile {
            profile: profile.to_owned(),
            app,
        })
    };
    let transport = if has(HEADLESS_BUNDLE) {
        Transport::None
    } else if has(WEB_BUNDLE) {
        Transport::Web
    } else if has(ACP_BUNDLE) {
        Transport::Acp
    } else if SDK_BUNDLES.iter().any(|sdk| has(sdk)) {
        return refuse(UnheldApp::Sdk);
    } else {
        return refuse(UnheldApp::Other);
    };
    let apps = APP_BUNDLES.into_iter().filter(|app| has(app)).collect();
    Ok(Detected { transport, apps })
}

#[cfg(test)]
#[path = "profile/unit_tests.rs"]
mod tests;
