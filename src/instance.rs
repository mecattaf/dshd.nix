//! Instances on disk: create atomically, load, discover, compare a recipe,
//! update the `current` root and remove. The CLI acts on instances only
//! through this module; the runner reads them through the same loaders.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, RecipeDifference};
use crate::fsx::{self, Lock};
use crate::name::{self, InstanceDir, InstanceName, LOCK_FILE, MAX_LEN, StateRoot};
use crate::profile;
use crate::record::{EnvVar, InstanceRecord, LastRun, OsText, ProfileRef, Transport};
use crate::release::{self, Release};
use crate::tool::Tool;

mod staging;

/// Mode of `instance.json` and `web-port`: the record holds the captured
/// environment.
const RECORD_MODE: u32 = 0o600;
/// The kernel's source of fresh random UUIDs.
const UUID_SOURCE: &str = "/proc/sys/kernel/random/uuid";
/// The word a generated name starts with when the profile name has no
/// usable character.
const EMPTY_PROFILE_WORD: &str = "dsh";

/// Everything `run -- ARGV` resolved before touching disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    /// Name selected under the root lock before creation.
    pub name: InstanceName,
    /// The validated release.
    pub release: Release,
    /// `--release` was given, so an existing instance must record it.
    pub release_given: bool,
    /// Canonical cwd.
    pub cwd: PathBuf,
    /// Native argv after rewriting only the profile selection.
    pub args: Vec<OsString>,
    /// Captured environment after the closed list and `unsetEnvironment`.
    pub environment: Vec<EnvVar>,
    /// The source profile and its DSH home.
    pub profile: ProfileRef,
    /// Detected transport.
    pub transport: Transport,
    /// Labels, without empty values.
    pub labels: BTreeMap<String, String>,
    /// `--patch` files from argv, for the composition stamp. A relative
    /// path is taken against `cwd`, where `dsh` will read it.
    pub patches: Vec<PathBuf>,
}

/// The instance directories on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discovery {
    /// The instances, sorted.
    pub names: Vec<InstanceName>,
    /// One line per directory whose name breaks the grammar.
    pub invalid: Vec<String>,
}

/// A loaded instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// The name.
    pub name: InstanceName,
    /// Its state directory.
    pub dir: InstanceDir,
    /// Its record.
    pub record: InstanceRecord,
}

/// Creates the instance directory atomically: build under a temp directory
/// beside the final one (`profile` copy, `gcroots`, `instance.json` 0600),
/// expose the profile link, rename into place, then register the
/// `current` and `booted` roots at their final paths (an indirect root
/// names its link's path, so it is made after the rename). Any failure
/// removes what was built. A name that exists is
/// `Error::ExistsDifferentRecipe` after comparison, or `Ok` with the
/// existing instance when the recipe matches; an existing record dshd
/// would not act on is `InconsistentState`. The caller holds
/// [`lock_root`].
pub fn create(
    root: &StateRoot,
    nix_store: &Tool,
    request: CreateRequest,
) -> Result<Instance, Error> {
    let name = request.name.clone();
    let dir = root.instance(&name);
    if fsx::kind(dir.path())?.is_some() {
        let existing = load_strict(root, &name)?;
        return match compare_recipe(&existing.record, &request) {
            None => Ok(existing),
            Some(difference) => Err(Error::ExistsDifferentRecipe(Box::new(difference))),
        };
    }
    staging::build(root, nix_store, name, request)
}

/// `<profile>-<k>` for the smallest k >= 1 not taken on disk or loaded in
/// the manager. `taken` is the union the caller gathered. The profile word
/// is lowercased, characters outside the name grammar become `-`, leading
/// non-alphanumerics are dropped and it is shortened to fit.
fn generate_name(profile: &str, taken: &[InstanceName]) -> Result<InstanceName, Error> {
    let word = name_word(profile);
    let mut k: u64 = 1;
    let mut candidate = numbered(&word, k);
    while taken.iter().any(|name| name.as_str() == candidate) {
        k += 1;
        candidate = numbered(&word, k);
    }
    InstanceName::parse(&candidate)
}

/// Selects the lowest free generated name, avoiding state directories,
/// loaded units and exposed profile entries. The caller holds [`lock_root`].
pub fn choose_name(
    root: &StateRoot,
    loaded: &[InstanceName],
    dsh_home: &Path,
    profile_name: &str,
) -> Result<InstanceName, Error> {
    let mut taken = discover(root)?.names;
    taken.extend_from_slice(loaded);
    let prefix = format!("{profile_name}@");
    for (entry, _) in &fsx::list_dir(&profile::profiles_dir(dsh_home))? {
        let suffix = entry
            .to_str()
            .and_then(|entry| entry.strip_prefix(prefix.as_str()));
        if let Some(Ok(name)) = suffix.map(InstanceName::parse) {
            taken.push(name);
        }
    }
    generate_name(profile_name, &taken)
}

/// Loads an instance, keeping unrecognized keys. `NotFound` when the
/// directory is absent; `InconsistentState` when the directory exists but
/// the record is unreadable.
pub fn load(root: &StateRoot, name: &InstanceName) -> Result<Instance, Error> {
    let dir = root.instance(name);
    if fsx::kind(dir.path())?.is_none() {
        return Err(Error::NotFound(name.to_string()));
    }
    let inconsistent = |reason: String| Error::InconsistentState {
        name: name.to_string(),
        reason,
    };
    let record = match fsx::read_json::<InstanceRecord>(&dir.instance_json()) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Err(inconsistent(format!(
                "{} is missing",
                dir.instance_json().display()
            )));
        }
        Err(error) => return Err(inconsistent(error.to_string())),
    };
    Ok(Instance {
        name: name.clone(),
        dir,
        record,
    })
}

/// [`load`], then `InconsistentState` when the record has unrecognized keys
/// or its `name` differs from the directory. Readers that act use this.
pub fn load_strict(root: &StateRoot, name: &InstanceName) -> Result<Instance, Error> {
    let instance = load(root, name)?;
    instance.require_known()?;
    Ok(instance)
}

impl Instance {
    /// Refuses a partial recipe from a newer writer or a mismatched name.
    /// Acting readers call this before using the record's fields.
    pub fn require_known(&self) -> Result<(), Error> {
        let inconsistent = |reason: String| Error::InconsistentState {
            name: self.name.to_string(),
            reason,
        };
        let keys = self.record.unrecognized_keys();
        if !keys.is_empty() {
            let reason = format!(
                "instance.json has unrecognized keys ({}): a newer dshd wrote it",
                keys.join(", ")
            );
            return Err(inconsistent(reason));
        }
        if self.record.name != self.name.as_str() {
            return Err(inconsistent(format!(
                "instance.json names {:?}",
                self.record.name
            )));
        }
        Ok(())
    }
}

/// The instance directories on disk, sorted. Dot entries (staging
/// directories) and files (the root lock) are skipped; a directory whose
/// name breaks the grammar is reported in `invalid`, one line each, and
/// never stops the listing.
pub fn discover(root: &StateRoot) -> Result<Discovery, Error> {
    let mut found = Discovery::default();
    let instances = root.instances_dir();
    for (entry, file_type) in &fsx::list_dir(&instances)? {
        let shown = OsText::new(entry.clone()).display();
        if !file_type.is_dir() || shown.starts_with('.') {
            continue;
        }
        match InstanceName::parse(&shown) {
            Ok(name) => found.names.push(name),
            Err(error) => found.invalid.push(format!(
                "{} is not an instance: {error}",
                instances.join(entry).display()
            )),
        }
    }
    Ok(found)
}

/// Compares a stored record with a request. `None` when equal in cwd,
/// args, profile and transport, and in the release name when the request
/// gave one. The environment and the labels are not part of the recipe:
/// the environment is captured once, at creation, and labels are set on
/// any match.
fn compare_recipe(stored: &InstanceRecord, request: &CreateRequest) -> Option<RecipeDifference> {
    let mut fields = Vec::new();
    let cwd = OsText::new(request.cwd.as_os_str());
    if stored.cwd != cwd {
        fields.push(("cwd", stored.cwd.display(), cwd.display()));
    }
    let args: Vec<OsText> = request.args.iter().cloned().map(OsText::from).collect();
    if stored.args != args {
        fields.push(("args", show_args(&stored.args), show_args(&args)));
    }
    if stored.profile != request.profile {
        fields.push((
            "profile",
            show_profile(&stored.profile),
            show_profile(&request.profile),
        ));
    }
    if stored.transport != request.transport {
        fields.push((
            "transport",
            stored.transport.slug().to_owned(),
            request.transport.slug().to_owned(),
        ));
    }
    if request.release_given && stored.release != request.release.name {
        fields.push((
            "release",
            stored.release.clone(),
            request.release.name.clone(),
        ));
    }
    if fields.is_empty() {
        None
    } else {
        Some(RecipeDifference {
            name: stored.name.clone(),
            fields,
        })
    }
}

/// Sets `labels` on the record, an empty value removing its key, and
/// writes `instance.json` (0600) atomically. Returns `false` when nothing
/// changed, and writes nothing then. The caller holds [`lock`].
pub fn set_labels(instance: &Instance, labels: &BTreeMap<String, String>) -> Result<bool, Error> {
    let mut record = instance.record.clone();
    for (key, value) in labels {
        if value.is_empty() {
            record.labels.remove(key);
        } else {
            record.labels.insert(key.clone(), value.clone());
        }
    }
    if record.labels == instance.record.labels {
        return Ok(false);
    }
    fsx::write_json_atomic(&instance.dir.instance_json(), &record, RECORD_MODE)?;
    Ok(true)
}

/// Removes newly excluded environment names from the persisted recipe.
/// The caller holds the instance lock. Returns removed names only, never
/// their values; no write occurs when the environment was already clean.
pub fn strip_environment(instance: &Instance, unset: &[String]) -> Result<Vec<String>, Error> {
    let mut record = instance.record.clone();
    let mut removed = Vec::new();
    record.environment.retain(|variable| {
        let drop = unset
            .iter()
            .any(|name| variable.name.as_os_str() == name.as_str());
        if drop {
            removed.push(variable.name.display());
        }
        !drop
    });
    if !removed.is_empty() {
        fsx::write_json_atomic(&instance.dir.instance_json(), &record, RECORD_MODE)?;
    }
    removed.sort();
    removed.dedup();
    Ok(removed)
}

/// Repoints `current` at the release and refreshes the profile's gcroots.
/// Returns `false` when `current` already pointed there. `nix-store`
/// replaces the link itself (temp link, rename) and registers the root at
/// the link's own path.
pub fn update_current(
    instance: &Instance,
    nix_store: &Tool,
    release: &Release,
) -> Result<bool, Error> {
    let current = instance.dir.current();
    let changed = fsx::read_link(&current)?.as_deref() != Some(release.path.as_path());
    if changed {
        release::add_root(nix_store, &release.path, &current)?;
    }
    staging::refresh_gcroots(&instance.dir, nix_store)?;
    Ok(changed)
}

/// Removes the profile link (only when it points at this instance's copy),
/// then the directory tree. The caller has checked the unit is not loaded.
pub fn remove(instance: &Instance) -> Result<(), Error> {
    let dsh_home = Path::new(instance.record.profile.dsh_home.as_os_str());
    let exposed = instance.name.exposed_profile(&instance.record.profile.name);
    profile::unlink_exact(dsh_home, &exposed, &instance.dir.profile())?;
    fsx::remove_tree(instance.dir.path())
}

/// Takes the instance lock (`%S/dshd/<name>/lock`). A creation takes the
/// root lock instead, through [`lock_root`].
pub fn lock(dir: &InstanceDir) -> Result<Lock, Error> {
    fsx::lock_exclusive(&dir.lock())
}

/// Takes the instance lock, then loads the record with [`load_strict`]
/// under it, so the record read is the one the caller acts on. An absent
/// directory is `NotFound`.
pub fn lock_loaded(root: &StateRoot, name: &InstanceName) -> Result<(Lock, Instance), Error> {
    let lock = match lock(&root.instance(name)) {
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Err(Error::NotFound(name.to_string()));
        }
        taken => taken?,
    };
    Ok((lock, load_strict(root, name)?))
}

/// Takes the root lock used around name generation and creation, creating
/// `%S/dshd` (0700) on first use.
pub fn lock_root(root: &StateRoot) -> Result<Lock, Error> {
    let instances = root.instances_dir();
    fsx::create_dir_private(&instances)?;
    fsx::lock_exclusive(&instances.join(LOCK_FILE))
}

/// The pinned web port, when written.
pub fn read_web_port(dir: &InstanceDir) -> Result<Option<u16>, Error> {
    let path = dir.web_port();
    let Some(text) = fsx::read_text(&path)? else {
        return Ok(None);
    };
    match text.trim().parse::<u16>() {
        Ok(port) => Ok(Some(port)),
        Err(e) => Err(Error::io(
            "parse",
            path,
            io::Error::new(io::ErrorKind::InvalidData, e),
        )),
    }
}

/// Pins the web port (0600).
pub fn write_web_port(dir: &InstanceDir, port: u16) -> Result<(), Error> {
    fsx::write_atomic(&dir.web_port(), format!("{port}\n").as_bytes(), RECORD_MODE)
}

/// The last run, when the instance ran at least once.
pub fn read_last_run(dir: &InstanceDir) -> Result<Option<LastRun>, Error> {
    fsx::read_json(&dir.last_run_json())
}

/// A fresh UUID from `/proc/sys/kernel/random/uuid`.
pub fn new_id() -> Result<String, Error> {
    let source = Path::new(UUID_SOURCE);
    let Some(text) = fsx::read_text(source)? else {
        return Err(Error::io(
            "read",
            source,
            io::Error::from(io::ErrorKind::NotFound),
        ));
    };
    let id = text.trim();
    if name::id_shaped(id) {
        Ok(id.to_owned())
    } else {
        Err(Error::io(
            "parse",
            source,
            io::Error::from(io::ErrorKind::InvalidData),
        ))
    }
}

fn name_word(profile: &str) -> String {
    let mapped: String = profile
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let word = mapped.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if word.is_empty() {
        EMPTY_PROFILE_WORD.to_owned()
    } else {
        word.to_owned()
    }
}

/// `<word>-<k>`, with the word shortened so the whole fits the grammar.
fn numbered(word: &str, k: u64) -> String {
    let suffix = format!("-{k}");
    let room = MAX_LEN.saturating_sub(suffix.len());
    let head: String = word.chars().take(room).collect();
    format!("{head}{suffix}")
}

fn show_args(args: &[OsText]) -> String {
    let shown: Vec<String> = args.iter().map(OsText::display).collect();
    format!("{shown:?}")
}

fn show_profile(profile: &ProfileRef) -> String {
    format!("{} in {}", profile.name, profile.dsh_home.display())
}

#[cfg(test)]
#[path = "instance/unit_tests.rs"]
mod tests;
