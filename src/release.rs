//! Releases: the registry under `/etc/dshd/releases`, validation of a
//! release package, the `current` and `booted` indirect GC roots, and the
//! four release states `status` reports.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::error::Error;
use crate::fsx;
use crate::name::{
    DEFAULT_RELEASE, InstanceDir, InstanceName, RELEASE_DSH, RELEASE_FILE, RELEASE_RUNNER,
    StateRoot, USER_RELEASES_DIR,
};
use crate::record::ReleaseFile;
use crate::tool::Tool;

/// The Nix store. Release paths live directly under it.
const STORE_DIR: &str = "/nix/store";
/// Length of the hash part of a store path's name.
const STORE_HASH_LEN: usize = 32;
/// Store name prefix of a package built by `lib.mkRelease`.
const RELEASE_NAME_PREFIX: &str = "dshd-release-";
/// Any of the execute bits.
const EXECUTE_BITS: u32 = 0o111;
/// Why a release path outside the Nix store is refused.
const NOT_A_STORE_PATH: &str = "it is not a store path (a directory directly under /nix/store)";

/// A resolved, validated release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Registry name the release was resolved from.
    pub name: String,
    /// The store path (registry link target, canonical).
    pub path: PathBuf,
    /// Its `share/dshd/release.json`.
    pub file: ReleaseFile,
}

/// How an instance's links relate to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    /// `current` equals the registry and `booted` equals `current` (or the
    /// instance never ran).
    Current,
    /// `current` equals the registry but the running `booted` differs: a
    /// restart applies it.
    Stale,
    /// `current` differs from the registry: `dshd update` applies it.
    Outdated,
    /// The registry name is gone or `current` dangles.
    Missing,
}

impl ReleaseState {
    /// The slug for JSON and the human label.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Outdated => "outdated",
            Self::Missing => "missing",
        }
    }
}

/// Where release names resolve, and where the releases that need no Nix
/// live. A name resolves through the system registry when that holds it at
/// all, and through the user registry only otherwise, so a NixOS or
/// nix-darwin host, which never writes the user registry, resolves as it
/// always has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registries {
    /// `/etc/dshd/releases`, written by the NixOS module.
    pub system: PathBuf,
    /// `<config root>/dshd/releases`, written by `dshd release add`; `None`
    /// when the manager names no configuration root.
    pub user: Option<PathBuf>,
    /// The directory store, `%S/dshd/.releases`.
    pub store: PathBuf,
}

impl Registries {
    /// The registries of a user: `system` as given, the user registry under
    /// `config_root`, the directory store under the instances directory of
    /// `state_root`.
    pub fn new(system: &Path, config_root: Option<&Path>, state_root: &StateRoot) -> Self {
        Self {
            system: system.to_path_buf(),
            user: config_root.map(|root| root.join(USER_RELEASES_DIR)),
            store: directory_store(state_root),
        }
    }

    /// Only the system registry, as before the directory store: no user
    /// registry, and a store no path can be an entry of.
    pub fn system_only(system: &Path) -> Self {
        Self {
            system: system.to_path_buf(),
            user: None,
            store: PathBuf::new(),
        }
    }
}

/// The directory store of a user: `.releases` in the instances directory
/// (`%S/dshd/.releases`), a dot directory the instance listing skips.
/// `store::store_root` joins its argument's own path, so it is handed the
/// instances directory.
pub fn directory_store(state_root: &StateRoot) -> PathBuf {
    store::store_root(&StateRoot::new(state_root.instances_dir()))
}

/// Resolves the registry link `<RELEASES_DIR>/<name>` to its canonical
/// store path. `Ok(None)` when absent, dangling, or when `name` cannot be a
/// registry name. A `name` that is an absolute path (a release pinned by
/// path) resolves to itself, so such an instance is never `outdated`.
pub fn registry_target(releases_dir: &Path, name: &str) -> Result<Option<PathBuf>, Error> {
    if name.starts_with('/') {
        return fsx::canonical_path(Path::new(name));
    }
    if InstanceName::parse(name).is_err() {
        return Ok(None);
    }
    fsx::canonical_path(&releases_dir.join(name))
}

/// [`registry_target`] through both registries: the system registry's link
/// when it holds `name`, a dangling link included, else the user
/// registry's.
pub fn registry_target_in(registries: &Registries, name: &str) -> Result<Option<PathBuf>, Error> {
    match registry_link(registries, name)? {
        Some(link) => fsx::canonical_path(&link),
        None => registry_target(&registries.system, name),
    }
}

/// The user registry link `name` resolves through: `None` for a path, a
/// name outside the grammar, a name the system registry holds, or no user
/// registry.
fn registry_link(registries: &Registries, name: &str) -> Result<Option<PathBuf>, Error> {
    if name.starts_with('/') || InstanceName::parse(name).is_err() {
        return Ok(None);
    }
    if fsx::kind(&registries.system.join(name))?.is_some() {
        return Ok(None);
    }
    Ok(registries.user.as_ref().map(|user| user.join(name)))
}

/// [`resolve`] through both registries, accepting an entry of the
/// directory store by its path as well as a Nix store path.
pub fn resolve_in(registries: &Registries, name: Option<&str>) -> Result<Release, Error> {
    let name = name.unwrap_or(DEFAULT_RELEASE);
    if name.starts_with('/') {
        return resolve_path(registries, name);
    }
    let Some(path) = registry_target_in(registries, name)? else {
        return Err(Error::ReleaseNotFound(name.to_owned()));
    };
    let file = validate(&path)?;
    Ok(Release {
        name: name.to_owned(),
        path,
        file,
    })
}

/// Whether `path`, canonical, is an entry of the directory store at
/// `store`.
pub fn is_directory_entry(store: &Path, path: &Path) -> bool {
    if store.as_os_str().is_empty() {
        return false;
    }
    match fsx::canonical_path(store) {
        Ok(Some(root)) => store::is_entry(&root, path),
        _ => false,
    }
}

/// Whether `path` lies in the Nix store.
pub fn in_nix_store(path: &Path) -> bool {
    store_path_of(path).is_some()
}

/// Whether `path` is a release of a directory store, by where it lives:
/// outside the Nix store, named `<32 hex>-dshd-release-<label>` in a
/// directory named `.releases`. Its roots are plain links, and it has no
/// closure to compare.
pub fn in_directory_store(path: &Path) -> bool {
    !in_nix_store(path) && entry_shaped(path)
}

/// Resolves `name` (or `default`) through the registry and validates the
/// package: `bin/dsh` and `bin/dshd-run` executable, `release.json` parsed
/// with no unrecognized keys. A `name` that is an absolute path under
/// `/nix/store` is used directly, with the path as the release name, so a
/// release needs no registry entry to be tried.
pub fn resolve(releases_dir: &Path, name: Option<&str>) -> Result<Release, Error> {
    resolve_in(&Registries::system_only(releases_dir), name)
}

/// Validates a release package at `path` and loads its metadata.
pub fn validate(path: &Path) -> Result<ReleaseFile, Error> {
    let invalid = |reason: String| Error::ReleaseInvalid {
        release: path.display().to_string(),
        reason,
    };
    for entry in [RELEASE_DSH, RELEASE_RUNNER] {
        let executable = fsx::metadata(&path.join(entry))?.is_some_and(|metadata| {
            metadata.is_file() && (metadata.permissions().mode() & EXECUTE_BITS) != 0
        });
        if !executable {
            return Err(invalid(format!("{entry} is missing or not executable")));
        }
    }
    let file = match fsx::read_json::<ReleaseFile>(&path.join(RELEASE_FILE)) {
        Ok(Some(file)) => file,
        Ok(None) => return Err(invalid(format!("{RELEASE_FILE} is missing"))),
        Err(error) => return Err(invalid(error.to_string())),
    };
    if !file.unrecognized.is_empty() {
        let keys: Vec<&str> = file.unrecognized.keys().map(String::as_str).collect();
        return Err(invalid(format!(
            "{RELEASE_FILE} has unrecognized keys: {}",
            keys.join(", ")
        )));
    }
    Ok(file)
}

/// `nix-store --option substitute false --realise PATH --add-root LINK`:
/// an indirect root that survives GC. An entry of a directory store is
/// not Nix's to collect: `LINK` becomes a plain link to it, and no
/// `nix-store` runs.
pub fn add_root(nix_store: &Tool, store_path: &Path, link: &Path) -> Result<(), Error> {
    if in_directory_store(store_path) {
        return fsx::symlink_replace(store_path, link);
    }
    nix_store.run([
        OsStr::new("--option"),
        OsStr::new("substitute"),
        OsStr::new("false"),
        OsStr::new("--realise"),
        store_path.as_os_str(),
        OsStr::new("--add-root"),
        link.as_os_str(),
    ])?;
    let target = fsx::read_link(link)?;
    if target.as_deref() == Some(store_path) {
        return Ok(());
    }
    Err(Error::ReleaseInvalid {
        release: store_path.display().to_string(),
        reason: format!("nix-store did not point {} at it", link.display()),
    })
}

/// Releases a stopped instance's old booted closure by pointing its stable
/// indirect root at `current`. No Nix process is needed: the auto root
/// already follows this booted link's path.
pub fn collapse_booted(dir: &InstanceDir) -> Result<bool, Error> {
    let current = fsx::read_link(&dir.current())?.ok_or_else(|| {
        Error::io(
            "read link",
            dir.current(),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        )
    })?;
    if fsx::read_link(&dir.booted())?.as_ref() == Some(&current) {
        return Ok(false);
    }
    fsx::symlink_replace(&current, &dir.booted())?;
    Ok(true)
}

/// Derives the release state from the three link targets.
pub fn release_state(
    current: Option<&Path>,
    booted: Option<&Path>,
    registry: Option<&Path>,
) -> ReleaseState {
    match (current, registry) {
        (None, _) | (_, None) => ReleaseState::Missing,
        (Some(current), Some(registry)) if current != registry => ReleaseState::Outdated,
        (Some(current), Some(_)) if booted.is_some_and(|booted| booted != current) => {
            ReleaseState::Stale
        }
        (Some(_), Some(_)) => ReleaseState::Current,
    }
}

/// Reads an instance's `current` and `booted` targets, canonical. A link
/// that is absent or dangles is `None`.
pub fn links(dir: &InstanceDir) -> Result<(Option<PathBuf>, Option<PathBuf>), Error> {
    Ok((
        fsx::canonical_path(&dir.current())?,
        fsx::canonical_path(&dir.booted())?,
    ))
}

/// Short label from a release store path: the store name without its hash
/// and without the `dshd-release-` prefix (`dsh-0.2.2-rc.1`).
pub fn version_label(path: &Path) -> String {
    let Some(base) = path.file_name().and_then(OsStr::to_str) else {
        return path.display().to_string();
    };
    let unhashed = match base.split_once('-') {
        Some((hash, rest)) if is_store_hash(hash) => rest,
        _ => base,
    };
    if let Some(label) = unhashed.strip_prefix(RELEASE_NAME_PREFIX) {
        label.to_owned()
    } else {
        unhashed.to_owned()
    }
}

/// The lines `nix store diff-closures OLD NEW` prints: the packages whose
/// version or size the update changes.
pub fn closure_difference(nix: &Tool, old: &Path, new: &Path) -> Result<Vec<String>, Error> {
    let captured = nix.run([
        OsStr::new("--extra-experimental-features"),
        OsStr::new("nix-command"),
        OsStr::new("store"),
        OsStr::new("diff-closures"),
        old.as_os_str(),
        new.as_os_str(),
    ])?;
    Ok(captured.stdout_text().lines().map(str::to_owned).collect())
}

/// The top-level store path that `path` lies in, when it lies in the store.
fn store_path_of(path: &Path) -> Option<PathBuf> {
    let Ok(inside) = path.strip_prefix(STORE_DIR) else {
        return None;
    };
    match inside.components().next() {
        Some(Component::Normal(name)) => Some(Path::new(STORE_DIR).join(name)),
        _ => None,
    }
}

/// Whether `path` has the shape of a directory store entry: named
/// `<32 hex>-dshd-release-<label>` in a directory named `.releases`.
fn entry_shaped(path: &Path) -> bool {
    let in_store = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == store::STORE_DIR);
    let named = path
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.split_once(store::ENTRY_INFIX))
        .is_some_and(|(hash, label)| {
            hash.len() == STORE_HASH_LEN
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && !label.is_empty()
        });
    in_store && named
}

/// A release given by its path: a Nix store path, or an entry of the
/// directory store.
fn resolve_path(registries: &Registries, given: &str) -> Result<Release, Error> {
    if let Some(path) = fsx::canonical_path(Path::new(given))?
        && !in_nix_store(&path)
        && is_directory_entry(&registries.store, &path)
    {
        let Some(name) = path.to_str().map(str::to_owned) else {
            return Err(Error::ReleaseInvalid {
                release: given.to_owned(),
                reason: "its path is not UTF-8".to_owned(),
            });
        };
        let file = validate(&path)?;
        return Ok(Release { name, path, file });
    }
    resolve_store_path(given).map_err(|error| match error {
        Error::ReleaseInvalid { release, reason }
            if reason == NOT_A_STORE_PATH && registries.store.is_dir() =>
        {
            Error::ReleaseInvalid {
                release,
                reason: format!(
                    "{reason}, nor an entry of the release store {}",
                    registries.store.display()
                ),
            }
        }
        other => other,
    })
}

fn resolve_store_path(given: &str) -> Result<Release, Error> {
    let invalid = |reason: &str| Error::ReleaseInvalid {
        release: given.to_owned(),
        reason: reason.to_owned(),
    };
    let Some(path) = fsx::canonical_path(Path::new(given))? else {
        return Err(invalid("it does not exist"));
    };
    if store_path_of(&path).as_deref() != Some(path.as_path()) {
        return Err(invalid(NOT_A_STORE_PATH));
    }
    let Some(name) = path.to_str().map(str::to_owned) else {
        return Err(invalid("its path is not UTF-8"));
    };
    let file = validate(&path)?;
    Ok(Release { name, path, file })
}

fn is_store_hash(text: &str) -> bool {
    text.len() == STORE_HASH_LEN
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

pub mod store;
pub mod user;

#[cfg(test)]
#[path = "release/unit_tests.rs"]
mod tests;
