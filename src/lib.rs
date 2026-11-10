//! dshd owns one thing: durable ownership of ordinary `dsh` processes under
//! the systemd user manager. Each instance is one unit, `dshd@<name>.service`,
//! whose main process is the per-release runner `dshd-run`, which spawns `dsh`
//! exactly as a terminal would and holds its pipes for the unit's lifetime.
//!
//! The crate has two binaries. `dshd` is the lifecycle CLI (`run`, `start`,
//! `stop`, `restart`, `update`, `ls`, `get`, `read`, `wait`, `attach`,
//! `label`, `enable`, `disable`, `rm`, `help`, `--skill`). `dshd-run` is the
//! runner (`run`, `poststop`) and the only ACP relay client (`attach`,
//! `probe`). Both read the same records and derive every name, unit and path
//! from one instance name.
//!
//! Nothing here supervises, schedules, logs or sandboxes. systemd owns the
//! process, journald owns the output, Nix roots the release bytes, and DSH
//! owns its sessions, settings, credentials and plugins.

/// The `dshd` verbs: parsing, output formats and each verb's body.
pub mod cli;
/// The one error type, with its kind slugs and exit codes.
pub mod error;
/// File-system primitives: atomic writes, locks, private directories.
pub mod fsx;
/// Instances on disk: creation, loading, discovery and removal.
pub mod instance;
/// The launch recipe: DSH's argv grammar, the environment, the DSH home.
pub mod launch;
/// Instance names and the unit, state and runtime paths derived from one.
pub mod name;
/// The only writer of stdout and stderr.
pub mod out;
/// Profiles: the source in a DSH home, the instance's copy and its link.
pub mod profile;
/// The on-disk records and their serde shapes.
pub mod record;
/// The ACP relay inside the runner, and its only client.
pub mod relay;
/// Releases: the registry, validation, the GC roots and the release states.
pub mod release;
/// The runner, the main process of each instance's unit.
pub mod runner;
/// The libc boundary: signal masks, the runner's kill and the user database.
#[allow(
    unsafe_code,
    reason = "the only module that names libc: signal masks, sigwait, kill of the runner's own child, and getpwuid_r"
)]
pub mod sys;
/// The user manager as dshd sees it, and the state derived from it.
pub mod systemctl;
/// Running external tools: `PATH` lookup, the C locale and `tool-missing`.
pub mod tool;
