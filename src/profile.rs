//! Profiles: where the source lives in a DSH home, how an instance gets its
//! own copy, how that copy is exposed back to DSH as `<p>@<name>`, which
//! transport the profile's bundles imply, and the stamp that lets `status`
//! say the copy was edited since the record.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::Metadata;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use crate::error::{Error, UnheldApp};
use crate::fsx;
use crate::launch::DSH_HOME;
use crate::record::{Composition, FileStamp, OsText, PatchStamp, Transport};
use crate::release;
use crate::tool::Tool;

/// Entries never copied: they are shared channels, not composition.
const SHARED_CHANNELS: &[&str] = &["cordis.yml", ".git"];

/// The profile manifest inside a profile directory.
const MANIFEST: &str = "package.json";

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
const SDK_BUNDLES: [&str; 2] = ["dsh-sdk-app", "dsh-sdk-minimal"];

/// Installed dependencies. They are copied, but not stamped: they follow
/// from the manifest and its lockfile, which are, and stamping every file
/// of a dependency tree would make the record as large as the tree.
const INSTALLED_DEPENDENCIES: &str = "node_modules";

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

/// `<dsh_home>/profiles`.
pub fn profiles_dir(dsh_home: &Path) -> PathBuf {
    dsh_home.join("profiles")
}

/// `<dsh_home>/profiles/<profile>`, canonical, so a profile linked into a
/// checkout is resolved once. `InvalidArgument` when the name is not a
/// plain directory name, when nothing is there, when the link dangles, or
/// when it is not a directory.
pub fn resolve_source(dsh_home: &Path, profile: &str) -> Result<PathBuf, Error> {
    let link = source_link(dsh_home, profile)?;
    let Some(source) = fsx::canonical_path(&link)? else {
        let reason = match fsx::read_link(&link) {
            Ok(Some(target)) => format!(
                "{} points at {}, which does not exist",
                link.display(),
                target.display()
            ),
            _ => format!("{} does not exist", link.display()),
        };
        return Err(Error::InvalidArgument(format!(
            "profile {profile}: {reason}"
        )));
    };
    if fsx::metadata(&source)?.is_some_and(|metadata| metadata.is_dir()) {
        Ok(source)
    } else {
        Err(Error::InvalidArgument(format!(
            "profile {profile}: {} is not a directory",
            source.display()
        )))
    }
}

/// Runs `dsh --profile <profile> --dump-default-config` so DSH materialises
/// its built-in profile (`web`, `acp`, `headless`) when the directory does
/// not exist yet. Returns the source directory.
pub fn initialise_source(dsh: &Tool, dsh_home: &Path, profile: &str) -> Result<PathBuf, Error> {
    let link = source_link(dsh_home, profile)?;
    if fsx::kind(&link)?.is_none() {
        let args = [
            OsString::from("--profile"),
            OsString::from(profile),
            OsString::from("--dump-default-config"),
        ];
        let mut command = dsh.command();
        command.args(&args).env(DSH_HOME, dsh_home);
        let output = command.output().map_err(|e| dsh.spawn_error(e))?;
        dsh.check(&args, output)?;
    }
    resolve_source(dsh_home, profile)
}

/// Copies `source` to `dest` (0700 directories, files with their modes plus
/// owner write), skipping `cordis.yml` and `.git` at the top of the profile.
/// A relative symlink that stays inside the profile is kept verbatim; any
/// other symlink points at its source-side target, absolute. Symlinks whose
/// target is under `/nix/store` are rooted under `gcroots/<k>` (one root per
/// store path, `k` from 1) through `nix-store --add-root`. Sockets, fifos and
/// devices are refused.
pub fn copy_profile(
    source: &Path,
    dest: &Path,
    gcroots: &Path,
    nix_store: &Tool,
) -> Result<(), Error> {
    let mut copier = Copier {
        source,
        gcroots,
        nix_store,
        rooted: Vec::new(),
    };
    copier.directory(source, dest, true)
}

/// Reads `package.json` `dsh.profile.bundles` of `profile` and maps them,
/// in this order: `dsh-headless` → `None`, `dsh-web-app` → `Web`,
/// `dsh-acp-app` → `Acp`. An SDK bundle, or a profile with none of these
/// (no manifest, no bundles, a terminal app), is `UnsupportedProfile`.
pub fn detect_transport(profile_dir: &Path, profile: &str) -> Result<Transport, Error> {
    let bundles = fsx::read_json::<Manifest>(&profile_dir.join(MANIFEST))?
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
    if has(HEADLESS_BUNDLE) {
        Ok(Transport::None)
    } else if has(WEB_BUNDLE) {
        Ok(Transport::Web)
    } else if has(ACP_BUNDLE) {
        Ok(Transport::Acp)
    } else if SDK_BUNDLES.iter().any(|sdk| has(sdk)) {
        refuse(UnheldApp::Sdk)
    } else {
        refuse(UnheldApp::Other)
    }
}

/// Links `<dsh_home>/profiles/<exposed>` → `profile_dir`. An existing link
/// with the same target is fine; anything else there is
/// `ProfileLinkExists`.
pub fn expose(dsh_home: &Path, exposed: &str, profile_dir: &Path) -> Result<(), Error> {
    let link = profiles_dir(dsh_home).join(exposed);
    match fsx::symlink(profile_dir, &link) {
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
            if verify_link(dsh_home, exposed, profile_dir)? {
                Ok(())
            } else {
                Err(Error::ProfileLinkExists(link))
            }
        }
        created => created,
    }
}

/// Removes the exposed link only when it points at `profile_dir`.
pub fn unlink_exact(dsh_home: &Path, exposed: &str, profile_dir: &Path) -> Result<(), Error> {
    // Anything else at that name belongs to the user and stays.
    fsx::unlink_if_target(&profiles_dir(dsh_home).join(exposed), profile_dir)?;
    Ok(())
}

/// `Ok(true)` when the link exists and points at `profile_dir`,
/// `Ok(false)` when it is absent, `ProfileLinkExists` when something else
/// is there.
pub fn verify_link(dsh_home: &Path, exposed: &str, profile_dir: &Path) -> Result<bool, Error> {
    let link = profiles_dir(dsh_home).join(exposed);
    match fsx::kind(&link)? {
        None => Ok(false),
        Some(kind)
            if kind.is_symlink() && fsx::read_link(&link)?.as_deref() == Some(profile_dir) =>
        {
            Ok(true)
        }
        Some(_) => Err(Error::ProfileLinkExists(link)),
    }
}

/// Stamps every regular file in the copy (relative path → size, mtime)
/// and the given patch files. The shared channels at the top, which DSH
/// rewrites itself, and installed dependency trees are left out.
pub fn composition_stamp(profile_dir: &Path, patches: &[PathBuf]) -> Result<Composition, Error> {
    let mut stamps = Vec::with_capacity(patches.len());
    for patch in patches {
        let Some(stamp) = stamp_file(patch)? else {
            return Err(Error::io(
                "stat",
                patch,
                io::Error::from(io::ErrorKind::NotFound),
            ));
        };
        stamps.push(PatchStamp {
            path: OsText::new(patch.as_os_str()),
            stamp,
            unrecognized: serde_json::Map::new(),
        });
    }
    Ok(Composition {
        files: stamp_tree(profile_dir)?,
        patches: stamps,
        unrecognized: serde_json::Map::new(),
    })
}

/// Whether the copy or a patch differs from the recorded stamp. A file or
/// patch that appeared or disappeared is a difference.
pub fn edited_since(profile_dir: &Path, recorded: &Composition) -> Result<bool, Error> {
    if stamp_tree(profile_dir)? != recorded.files {
        return Ok(true);
    }
    for patch in &recorded.patches {
        if stamp_file(Path::new(patch.path.as_os_str()))? != Some(patch.stamp.clone()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `<dsh_home>/profiles/<profile>`, after checking `profile` is one plain
/// path component.
fn source_link(dsh_home: &Path, profile: &str) -> Result<PathBuf, Error> {
    let mut components = Path::new(profile).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !profile.contains('/') => {
            Ok(profiles_dir(dsh_home).join(profile))
        }
        _ => Err(Error::InvalidArgument(format!(
            "profile name {profile:?} is not a directory name"
        ))),
    }
}

/// One profile copy in progress.
struct Copier<'a> {
    /// The source profile, the root that "inside the profile" refers to.
    source: &'a Path,
    /// Where the store roots go.
    gcroots: &'a Path,
    /// Registers the store roots.
    nix_store: &'a Tool,
    /// Store paths rooted so far; `gcroots/<k>` roots the k-th.
    rooted: Vec<PathBuf>,
}

impl Copier<'_> {
    fn directory(&mut self, from: &Path, to: &Path, top: bool) -> Result<(), Error> {
        fsx::create_dir_private(to)?;
        for (name, file_type) in &fsx::list_dir(from)? {
            if top && is_one_of(name, SHARED_CHANNELS) {
                continue;
            }
            let (from, to) = (from.join(name), to.join(name));
            if file_type.is_dir() {
                self.directory(&from, &to, false)?;
            } else if file_type.is_file() {
                fsx::copy_file(&from, &to)?;
            } else if file_type.is_symlink() {
                self.link(&from, &to)?;
            } else {
                return Err(Error::InvalidArgument(format!(
                    "cannot copy {}: a profile holds only files, directories and symlinks",
                    from.display()
                )));
            }
        }
        fsx::sync_directory(to)
    }

    fn link(&mut self, from: &Path, to: &Path) -> Result<(), Error> {
        let Some(target) = fsx::read_link(from)? else {
            return Ok(());
        };
        let absolute = from
            .parent()
            .map_or_else(|| target.clone(), |dir| lexical(&dir.join(&target)));
        if target.is_relative() && absolute.starts_with(self.source) {
            return fsx::symlink(&target, to);
        }
        fsx::symlink(
            if target.is_relative() {
                &absolute
            } else {
                &target
            },
            to,
        )?;
        let store_path =
            fsx::canonical_path(from)?.and_then(|resolved| release::store_path_of(&resolved));
        match store_path {
            Some(store_path) => self.root(store_path),
            None => Ok(()),
        }
    }

    fn root(&mut self, store_path: PathBuf) -> Result<(), Error> {
        if self.rooted.contains(&store_path) {
            return Ok(());
        }
        fsx::create_dir_private(self.gcroots)?;
        let link = self.gcroots.join((self.rooted.len() + 1).to_string());
        release::add_root(self.nix_store, &store_path, &link)?;
        fsx::sync_directory(self.gcroots)?;
        self.rooted.push(store_path);
        Ok(())
    }
}

/// `path` with `.` and `..` resolved lexically. Inside a profile the
/// parents are real directories (the walk never enters a linked one), so
/// this equals what the kernel resolves for the parent part.
fn lexical(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}

fn is_one_of(name: &OsStr, names: &[&str]) -> bool {
    names.iter().any(|candidate| name == OsStr::new(candidate))
}

/// Stamps of the regular files under `profile_dir`, keyed by relative path.
fn stamp_tree(profile_dir: &Path) -> Result<BTreeMap<String, FileStamp>, Error> {
    let mut stamps = BTreeMap::new();
    stamp_directory(profile_dir, Path::new(""), &mut stamps)?;
    Ok(stamps)
}

fn stamp_directory(
    dir: &Path,
    relative: &Path,
    stamps: &mut BTreeMap<String, FileStamp>,
) -> Result<(), Error> {
    let top = relative.as_os_str().is_empty();
    for (name, file_type) in &fsx::list_dir(dir)? {
        if (top && is_one_of(name, SHARED_CHANNELS)) || is_one_of(name, &[INSTALLED_DEPENDENCIES]) {
            continue;
        }
        let (path, relative) = (dir.join(name), relative.join(name));
        if file_type.is_dir() {
            stamp_directory(&path, &relative, stamps)?;
        } else if file_type.is_file()
            && let Some(stamp) = stamp_file(&path)?
        {
            // Keys are only compared with keys made the same way, so the
            // display form is enough for the rare non-UTF-8 name.
            stamps.insert(relative.display().to_string(), stamp);
        }
    }
    Ok(())
}

/// Size and mtime of what `path` resolves to. `Ok(None)` when absent.
fn stamp_file(path: &Path) -> Result<Option<FileStamp>, Error> {
    match fsx::metadata(path)? {
        Some(metadata) => file_stamp(path, &metadata).map(Some),
        None => Ok(None),
    }
}

fn file_stamp(path: &Path, metadata: &Metadata) -> Result<FileStamp, Error> {
    let modified = metadata
        .modified()
        .map_err(|e| Error::io("stat", path, e))?;
    let since_epoch = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::io("stat", path, io::Error::new(io::ErrorKind::InvalidData, e)))?;
    Ok(FileStamp {
        size: metadata.len(),
        mtime_secs: since_epoch.as_secs(),
        mtime_nanos: since_epoch.subsec_nanos(),
        unrecognized: serde_json::Map::new(),
    })
}

#[cfg(test)]
#[path = "profile/unit_tests.rs"]
mod tests;
