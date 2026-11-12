//! Instance names and everything derived from one: the unit name, the state
//! directory and the runtime directory. One name, one place to derive from
//! it, so no two modules can disagree about a path.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The unit template name. Instances are `dshd@<name>.service`.
pub const UNIT_TEMPLATE: &str = "dshd@.service";
/// The target that enabled instances are wanted by.
pub const TARGET: &str = "dshd.target";
/// Where the NixOS module installs the template; every wants link dshd
/// writes points here, so an instance always loads the current template.
pub const TEMPLATE_FILE: &str = "/etc/systemd/user/dshd@.service";
/// Longest accepted instance name.
pub const MAX_LEN: usize = 63;

/// File names inside an instance's state directory `%S/dshd/<name>`.
const INSTANCE_JSON: &str = "instance.json";
/// Record of the last run of the instance, written by `dshd-run poststop`.
const LAST_RUN_JSON: &str = "last-run.json";
/// Indirect GC root to the release the next start will use.
const CURRENT_LINK: &str = "current";
/// Indirect GC root to the release the running (or last) start used.
const BOOTED_LINK: &str = "booted";
/// Pinned web port for a web instance, written on first start.
const WEB_PORT_FILE: &str = "web-port";
/// Advisory lock taken by the CLI around create/update/remove.
pub const LOCK_FILE: &str = "lock";
/// The directory of an instance's `run --patch` snapshots.
const PATCHES_DIR: &str = "patches";

/// File names inside an instance's runtime directory `%t/dshd/<name>`.
const WEB_URL_FILE: &str = "web-url";
/// Unix socket the runner's ACP relay listens on.
const ACP_SOCKET_FILE: &str = "acp.sock";
/// Empty file `dshd stop` and `dshd restart` write before they ask systemd
/// to stop the unit; the runner's post-stop step reads and removes it.
pub const STOP_INTENT_FILE: &str = "stop-intent";

/// The release registry written by the NixOS module.
pub const RELEASES_DIR: &str = "/etc/dshd/releases";
/// The registry entry that `dshd run` uses when no `--release` is given.
pub const DEFAULT_RELEASE: &str = "default";
/// Paths inside a release package.
pub const RELEASE_DSH: &str = "bin/dsh";
/// The runner inside a release package.
pub const RELEASE_RUNNER: &str = "bin/dshd-run";
/// The release metadata file inside a release package.
pub const RELEASE_FILE: &str = "share/dshd/release.json";

/// The instance-name grammar as the help, the errors and the README state
/// it. The NixOS module and the fixture `tests/fixtures/instance-names.json`
/// hold the same rule.
pub const GRAMMAR: &str =
    "^[a-z0-9]([a-z0-9_-]{0,61}[a-z0-9])?$, and not shaped like an instance id (a UUID)";

/// A validated instance name, by [`GRAMMAR`]. Names and ids are disjoint,
/// so an id is never taken for a name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceName(String);

impl InstanceName {
    /// Validates `raw` against the grammar. The error names the first rule
    /// the name broke and its exit code is `invalid-name` (2).
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let bytes = raw.as_bytes();
        let inner =
            |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_';
        let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        let reason = if raw.is_empty() {
            Some("must not be empty")
        } else if bytes.len() > MAX_LEN {
            Some("must be at most 63 characters")
        } else if !bytes.iter().all(inner) {
            Some("may only contain a-z, 0-9, '-' and '_'")
        } else if !bytes.first().is_some_and(edge) || !bytes.last().is_some_and(edge) {
            Some("must start and end with a letter or digit")
        } else if id_shaped(raw) {
            Some("must not be shaped like an instance id (a UUID)")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(Error::InvalidName {
                name: raw.to_owned(),
                reason,
            }),
            None => Ok(Self(raw.to_owned())),
        }
    }

    /// The name as given.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `dshd@<name>.service`.
    pub fn unit(&self) -> String {
        format!("dshd@{}.service", self.0)
    }

    /// Parses a loaded unit name back into an instance name, or `None` for
    /// units that are not instances of the template.
    pub fn from_unit(unit: &str) -> Option<Self> {
        let inner = unit.strip_prefix("dshd@")?.strip_suffix(".service")?;
        Self::parse(inner).ok()
    }
}

impl fmt::Display for InstanceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The state root: `$XDG_STATE_HOME` of the manager, else `~/.local/state`.
/// systemd's `%S` for a user unit resolves to the same directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRoot(PathBuf);

impl StateRoot {
    /// Wraps an already-resolved state root.
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// `<root>/dshd`: the directory that holds one subdirectory per instance.
    pub fn instances_dir(&self) -> PathBuf {
        self.0.join("dshd")
    }

    /// The state directory of one instance.
    pub fn instance(&self, name: &InstanceName) -> InstanceDir {
        InstanceDir(self.instances_dir().join(name.as_str()))
    }

    /// The state root itself.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// `%S/dshd/<name>` and the files inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceDir(PathBuf);

impl InstanceDir {
    /// Wraps a path that is already the instance directory (the runner gets
    /// it from `WorkingDirectory`).
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// `instance.json`.
    pub fn instance_json(&self) -> PathBuf {
        self.0.join(INSTANCE_JSON)
    }

    /// `last-run.json`.
    pub fn last_run_json(&self) -> PathBuf {
        self.0.join(LAST_RUN_JSON)
    }

    /// The `current` release link.
    pub fn current(&self) -> PathBuf {
        self.0.join(CURRENT_LINK)
    }

    /// The `booted` release link.
    pub fn booted(&self) -> PathBuf {
        self.0.join(BOOTED_LINK)
    }

    /// The pinned web port file.
    pub fn web_port(&self) -> PathBuf {
        self.0.join(WEB_PORT_FILE)
    }

    /// The advisory lock file.
    pub fn lock(&self) -> PathBuf {
        self.0.join(LOCK_FILE)
    }

    /// The directory of the `run --patch` snapshots.
    pub fn patches(&self) -> PathBuf {
        self.0.join(PATCHES_DIR)
    }

    /// The snapshot recorded as `file`.
    pub fn patch(&self, file: &str) -> PathBuf {
        self.patches().join(file)
    }
}

/// `%t/dshd/<name>` and the files inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDir(PathBuf);

impl RuntimeDir {
    /// `<runtime root>/dshd/<name>`. The runtime root is `$XDG_RUNTIME_DIR`
    /// of the manager; the runner receives the directory itself in
    /// `$RUNTIME_DIRECTORY`.
    pub fn under(runtime_root: &Path, name: &InstanceName) -> Self {
        Self(runtime_root.join("dshd").join(name.as_str()))
    }

    /// Wraps a path that is already the runtime directory.
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The announced web URL (mode 0600, token included).
    pub fn web_url(&self) -> PathBuf {
        self.0.join(WEB_URL_FILE)
    }

    /// The ACP relay socket.
    pub fn acp_socket(&self) -> PathBuf {
        self.0.join(ACP_SOCKET_FILE)
    }

    /// The stop intent `dshd` leaves for the runner's post-stop step.
    pub fn stop_intent(&self) -> PathBuf {
        self.0.join(STOP_INTENT_FILE)
    }
}

/// Whether `text` has the shape of an instance id: a UUID in its lowercase
/// text form, groups of 8, 4, 4, 4 and 12 hex digits joined by `-`.
pub fn id_shaped(text: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == GROUPS.len()
        && groups.iter().zip(GROUPS).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// `argv[0]` of the runner, as the CLI invokes it through the `booted` link.
pub fn booted_runner(dir: &InstanceDir) -> OsString {
    dir.booted().join(RELEASE_RUNNER).into_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accept and reject lists the NixOS module's assertion reads too.
    const NAMES: &str = include_str!("../tests/fixtures/instance-names.json");

    fn listed(key: &str) -> Vec<String> {
        let names: serde_json::Value = serde_json::from_str(NAMES).unwrap();
        let list = names
            .get(key)
            .and_then(serde_json::Value::as_array)
            .unwrap();
        list.iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect()
    }

    // catches: the Rust validator and the shared fixture (which the Nix assertion reads) disagreeing on a name
    #[test]
    fn accepts_and_rejects_per_grammar() {
        let (valid, invalid) = (listed("valid"), listed("invalid"));
        assert!(!valid.is_empty() && !invalid.is_empty());
        for ok in &valid {
            assert!(InstanceName::parse(ok).is_ok(), "{ok:?}");
        }
        for bad in &invalid {
            assert!(InstanceName::parse(bad).is_err(), "{bad:?}");
        }
    }

    // catches: an instance id accepted as a name, so a name could later be read as an id
    #[test]
    fn ids_are_not_names() {
        let id = "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6f";
        assert!(id_shaped(id));
        match InstanceName::parse(id) {
            Err(Error::InvalidName { reason, .. }) => assert!(reason.contains("UUID"), "{reason}"),
            other => panic!("{other:?}"),
        }
        for near in [
            "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6",
            "4b2d6a5e1f3c4e7a9c0d2a1b3c4d5e6f",
            "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6g",
        ] {
            assert!(!id_shaped(near), "{near}");
            assert!(InstanceName::parse(near).is_ok(), "{near}");
        }
    }

    // catches: a unit name the template or the listing would not map back to the instance
    #[test]
    fn derives_unit_and_back() {
        let name = InstanceName::parse("web-1").unwrap();
        assert_eq!(name.unit(), "dshd@web-1.service");
        assert_eq!(
            InstanceName::from_unit("dshd@web-1.service"),
            Some(name.clone())
        );
        assert_eq!(InstanceName::from_unit("dshd.target"), None);
    }

    // catches: a state or runtime path that moved away from where the unit and the runner look
    #[test]
    fn derives_paths_from_root() {
        let root = StateRoot::new(PathBuf::from("/s"));
        let dir = root.instance(&InstanceName::parse("x").unwrap());
        assert_eq!(dir.path(), Path::new("/s/dshd/x"));
        assert_eq!(
            dir.instance_json(),
            PathBuf::from("/s/dshd/x/instance.json")
        );
        assert_eq!(
            dir.booted().join(RELEASE_RUNNER),
            PathBuf::from("/s/dshd/x/booted/bin/dshd-run")
        );
        let rt = RuntimeDir::under(Path::new("/srv/rt"), &InstanceName::parse("x").unwrap());
        assert_eq!(rt.acp_socket(), PathBuf::from("/srv/rt/dshd/x/acp.sock"));
        assert_eq!(
            rt.stop_intent(),
            PathBuf::from("/srv/rt/dshd/x/stop-intent")
        );
    }

    // catches: a longer socket file name or name limit that no longer fits sun_path (108 bytes with the NUL)
    #[test]
    fn longest_socket_path_fits_sun_path() {
        const SUN_PATH: usize = 108;
        let longest_root = Path::new("/run").join("user").join(u32::MAX.to_string());
        let name = InstanceName::parse(&"a".repeat(MAX_LEN)).unwrap();
        let socket = RuntimeDir::under(&longest_root, &name).acp_socket();
        assert!(
            socket.as_os_str().len() < SUN_PATH,
            "{} is {} bytes",
            socket.display(),
            socket.as_os_str().len()
        );
    }
}
