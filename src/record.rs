//! The on-disk records: `instance.json`, `last-run.json` and the release's
//! `release.json`. Objects with camelCase keys, no version marker. Unknown
//! keys are kept in `unrecognized` so a reader that only reports can pass
//! them through; a reader that acts (the runner, `update`, `remove`) refuses
//! a record whose `unrecognized` map is not empty.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

mod unknown;

/// The shell's status for an end it cannot express otherwise.
const SHELL_FAILURE: u8 = 1;
/// Base of the shell convention for a death by signal S: 128+S.
const SHELL_SIGNAL_BASE: u8 = 128;

/// Text that came from the OS (argv, environment, paths) and may not be
/// UTF-8. Serialised as a JSON string when it is UTF-8, otherwise as
/// `{"bytes":[..]}`, so nothing is lost or guessed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OsText(OsString);

impl OsText {
    /// Wraps OS text.
    pub fn new(text: impl Into<OsString>) -> Self {
        Self(text.into())
    }

    /// Borrows the OS text.
    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }

    /// Lossless display for human output: UTF-8 as is, otherwise the
    /// bytes shown with Rust's debug escapes.
    pub fn display(&self) -> String {
        match self.0.to_str() {
            Some(s) => s.to_owned(),
            None => format!("{:?}", self.0.as_bytes()),
        }
    }
}

impl From<OsString> for OsText {
    fn from(text: OsString) -> Self {
        Self(text)
    }
}

impl From<&str> for OsText {
    fn from(text: &str) -> Self {
        Self(OsString::from(text))
    }
}

#[derive(Serialize, Deserialize)]
struct Bytes {
    bytes: Vec<u8>,
}

impl Serialize for OsText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.to_str() {
            Some(s) => serializer.serialize_str(s),
            None => Bytes {
                bytes: self.0.as_bytes().to_vec(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for OsText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Bytes(Bytes),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Text(s) => Self(OsString::from(s)),
            Repr::Bytes(b) => Self(OsString::from_vec(b.bytes)),
        })
    }
}

/// One captured environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    /// The variable name.
    pub name: OsText,
    /// The variable value.
    pub value: OsText,
    /// Keys this version does not know; an acting reader refuses them.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// How the instance is reached. Derived from the profile's bundles at create
/// time and recorded; every start refuses a profile that selects another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// `dsh web`: the runner captures the announced URL.
    Web,
    /// `dsh acp`: the runner relays stdio through the ACP socket.
    Acp,
    /// Neither: `attach` refuses with `unsupported-transport`.
    None,
}

impl Transport {
    /// The transport's name, as records, JSON and the human output spell it.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Acp => "acp",
            Self::None => "none",
        }
    }
}

/// The DSH profile an instance runs, live from its DSH home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRef {
    /// The profile name as given to `dsh` (`web`, `acp`, a user profile).
    pub name: String,
    /// The DSH home the profile lives in, which the instance runs with.
    pub dsh_home: OsText,
    /// Keys this version does not know; an acting reader refuses them.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// One `--patch PATH` overlay dshd snapshotted into the instance at
/// creation: the copy under `%S/dshd/NAME/patches/` is what every start
/// hands dsh, and its digest is part of the recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPatch {
    /// File name under `%S/dshd/NAME/patches/`.
    pub file: String,
    /// Lowercase hex sha256 of the snapshot.
    pub sha256: String,
    /// The path as given at creation; informational, never read again.
    pub source: OsText,
    /// Keys this version does not know; an acting reader refuses them.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

impl RunPatch {
    /// The largest overlay `--patch` takes, in bytes (1 MiB).
    pub const MAX_BYTES: u64 = 1 << 20;
    /// Hex digits of the digest a snapshot's file name carries.
    const NAME_DIGITS: usize = 16;

    /// The snapshot of `bytes`, the `index`th overlay given, read from
    /// `source`: named `<NN>-<first 16 hex digits of its sha256>.yml`, so
    /// the names sort in the order the overlays were given.
    pub fn of(index: usize, bytes: &[u8], source: OsText) -> Self {
        let sha256 = Self::digest(bytes);
        let short: String = sha256.chars().take(Self::NAME_DIGITS).collect();
        Self {
            file: format!("{index:02}-{short}.yml"),
            sha256,
            source,
            unrecognized: serde_json::Map::new(),
        }
    }

    /// The lowercase hex sha256 of `bytes`.
    pub fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0x0f])
            .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
            .collect()
    }
}

/// The `run --stdin FILE` content dshd snapshotted into the instance at
/// creation: `%S/dshd/NAME/stdin` is what every start hands a headless
/// dsh as its stdin, and its digest is part of the recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStdin {
    /// Lowercase hex sha256 of the snapshot.
    pub sha256: String,
    /// The path as given at creation, `-` for dshd's own stdin;
    /// informational, never read again.
    pub source: OsText,
    /// Keys this version does not know; an acting reader refuses them.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

impl RunStdin {
    /// The largest content `--stdin` takes, in bytes (1 MiB, as `--patch`).
    pub const MAX_BYTES: u64 = RunPatch::MAX_BYTES;

    /// The snapshot of `bytes`, read from `source`.
    pub fn of(bytes: &[u8], source: OsText) -> Self {
        Self {
            sha256: RunPatch::digest(bytes),
            source,
            unrecognized: serde_json::Map::new(),
        }
    }
}

/// `instance.json`: the recipe, rewritten by labels and the update environment strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceRecord {
    /// Stable identity, a UUID from the kernel, never reused.
    pub id: String,
    /// The instance name (equals the directory name).
    pub name: String,
    /// The release registry name recorded at create time (`default` or a
    /// configured name). `update` follows it.
    pub release: String,
    /// Working directory `dsh` is started in.
    pub cwd: OsText,
    /// Native `dsh` arguments as typed. Web defaults and the snapshotted
    /// overlays are injected independently at each start.
    pub args: Vec<OsText>,
    /// The `run --patch` overlays snapshotted at creation, in the order
    /// given. Absent when there are none, so a record made without them
    /// is written exactly as before; a reader that does not know the key
    /// finds it unrecognized and refuses to act.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<RunPatch>,
    /// The `run --stdin` snapshot a headless dsh reads as its stdin at
    /// every start. Absent when none was given, so a record made without
    /// it is written exactly as before; a reader that does not know the
    /// key finds it unrecognized and refuses to act.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<RunStdin>,
    /// The environment captured at create time, after the closed exclusion
    /// list and the release's `unsetEnvironment` were applied.
    pub environment: Vec<EnvVar>,
    /// The profile the instance runs.
    pub profile: ProfileRef,
    /// How the instance is reached.
    pub transport: Transport,
    /// User labels from `--label KEY=VALUE`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Unix seconds when the record was created.
    pub created_at: u64,
    /// Keys this version does not know. Must be empty for a reader that acts.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// How the `dsh` child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildEnd {
    /// Exit code when the child exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal number when the child was killed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

impl ChildEnd {
    /// The status a shell reports for this end: the exit code as given,
    /// 128+S for a death by signal S (saturating at 255), and 1 when
    /// neither is set or the code fits no status.
    pub fn shell_status(self) -> u8 {
        match (self.exit_code, self.signal) {
            (Some(code), _) => u8::try_from(code).unwrap_or(SHELL_FAILURE),
            (None, Some(signal)) => u8::try_from(signal)
                .map_or(u8::MAX, |signal| SHELL_SIGNAL_BASE.saturating_add(signal)),
            (None, None) => SHELL_FAILURE,
        }
    }
}

/// `last-run.json`: the start half is written by `dshd-run run` before
/// `dsh` is spawned, with `end` null; `dshd-run poststop` completes it.
/// A record whose `end` is null therefore names a run that never ended
/// as far as dshd saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRun {
    /// The instance id at the time of the run.
    pub id: String,
    /// The store path of the release the run used (`booted` target).
    pub release_path: OsText,
    /// Boot id of the kernel the run happened under.
    pub boot_id: String,
    /// Unix seconds when the runner started.
    pub started_at: u64,
    /// The unit's `$INVOCATION_ID` for the run; empty outside systemd.
    pub invocation_id: String,
    /// How the run ended; null until `poststop` recorded it.
    pub end: Option<RunEnd>,
    /// Keys this version does not know.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// The end half of `last-run.json`: what systemd handed `ExecStopPost`
/// and what the runner saw itself, and nothing inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEnd {
    /// Unix seconds when `poststop` ran.
    pub ended_at: u64,
    /// The `SERVICE_RESULT` systemd handed to `ExecStopPost`.
    pub service_result: String,
    /// The `EXIT_CODE` systemd handed to `ExecStopPost` (`exited`, `killed`...).
    pub exit_code: String,
    /// The `EXIT_STATUS` systemd handed to `ExecStopPost`.
    pub exit_status: String,
    /// How the `dsh` child itself ended; null when the runner never saw it end.
    #[serde(default)]
    pub child: Option<ChildEnd>,
    /// dshd asked for this stop: the CLI wrote the runtime directory's
    /// stop-intent file before `systemctl stop` or `restart`.
    pub stop_intent: bool,
    /// The stop signal the runner received; null when none arrived.
    #[serde(default)]
    pub stop_signal: Option<i32>,
    /// What the run consumed; absent in a record an older `poststop` wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<RunResources>,
    /// Keys this version does not know.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// The run's totals, read by `poststop` from the unit's own cgroup once
/// every process of the run is gone; null where the controller is off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResources {
    /// CPU time in nanoseconds (`cpu.stat` `usage_usec`).
    #[serde(default, rename = "cpuUsageNSec")]
    pub cpu_usage_nsec: Option<u64>,
    /// Peak memory use in bytes (`memory.peak`).
    #[serde(default)]
    pub memory_peak: Option<u64>,
    /// Bytes read, over every device (`io.stat` `rbytes`).
    #[serde(default)]
    pub io_read_bytes: Option<u64>,
    /// Bytes written, over every device (`io.stat` `wbytes`).
    #[serde(default)]
    pub io_write_bytes: Option<u64>,
    /// Keys this version does not know.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// `share/dshd/release.json` inside a release package, written by
/// `lib.mkRelease`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseFile {
    /// Variables the release sets when the recorded environment lacks them.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Variables stripped from the captured environment at create time and
    /// from the composed environment at every start.
    #[serde(default)]
    pub unset_environment: Vec<String>,
    /// Keys this version does not know.
    #[serde(flatten)]
    pub unrecognized: serde_json::Map<String, serde_json::Value>,
}

/// Serialises a record as pretty JSON with a trailing newline, the only
/// shape dshd ever writes.
pub fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = serde_json::to_vec_pretty(value)?;
    out.push(b'\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: a snapshot digest that is not sha256 in lowercase hex, or a file name that does not sort in the
    // order the overlays were given
    #[test]
    fn run_patch_digest_and_name() {
        assert_eq!(
            RunPatch::digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(RunPatch::digest(b"abc"), abc);
        let patch = RunPatch::of(3, b"abc", OsText::from("p.yml"));
        assert_eq!(patch.file, format!("03-{}.yml", abc.get(..16).unwrap()));
        assert_eq!(
            RunPatch::of(12, b"", OsText::from("q")).file,
            "12-e3b0c44298fc1c14.yml"
        );
    }

    // catches: a stdin snapshot whose digest is not the content's sha256, or a record without one that no
    // longer serializes exactly as before
    #[test]
    fn run_stdin_digest_and_absent_key() {
        let stdin = RunStdin::of(b"abc", OsText::from("-"));
        assert_eq!(stdin.sha256, RunPatch::digest(b"abc"));
        let text = include_str!("../tests/fixtures/instance.json");
        let mut record: InstanceRecord = serde_json::from_str(text).unwrap();
        assert_eq!(record.stdin, None);
        let plain = String::from_utf8(to_json(&record).unwrap()).unwrap();
        assert!(!plain.contains("stdin"), "{plain}");
        record.stdin = Some(stdin.clone());
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json.get("stdin").unwrap(),
            &serde_json::json!({"sha256": stdin.sha256, "source": "-"})
        );
        let back: InstanceRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.stdin, Some(stdin));
        assert!(back.unrecognized.is_empty());
    }

    // catches: a transport named one way in records and JSON and another in messages and the human output
    #[test]
    fn transport_slug_is_its_serialized_name() {
        for transport in [Transport::Web, Transport::Acp, Transport::None] {
            assert_eq!(serde_json::to_value(transport).unwrap(), transport.slug());
        }
    }

    // catches: a path, argument or variable that is not UTF-8 changed or refused on its way through a record
    #[test]
    fn os_text_round_trips_both_shapes() {
        let text = OsText::from("plain");
        assert_eq!(serde_json::to_string(&text).unwrap(), "\"plain\"");
        let raw = OsText::new(OsString::from_vec(vec![0xff, b'a']));
        let json = serde_json::to_string(&raw).unwrap();
        assert_eq!(json, "{\"bytes\":[255,97]}");
        assert_eq!(serde_json::from_str::<OsText>(&json).unwrap(), raw);
        assert_eq!(
            serde_json::from_str::<OsText>("\"x\"").unwrap(),
            OsText::from("x")
        );
    }

    // catches: a record shape the fixtures no longer describe, or keys of a newer writer dropped on read
    #[test]
    fn fixtures_parse_and_keep_unknown_keys() {
        let text = include_str!("../tests/fixtures/instance.json");
        let record: InstanceRecord = serde_json::from_str(text).unwrap();
        assert_eq!(record.name, "web-1");
        assert_eq!(record.transport, Transport::Web);
        assert!(record.unrecognized.is_empty());
        let extra = text.replacen('{', "{\"colour\": 1,", 1);
        let record: InstanceRecord = serde_json::from_str(&extra).unwrap();
        assert_eq!(record.unrecognized.len(), 1);
        let last: LastRun =
            serde_json::from_str(include_str!("../tests/fixtures/last-run.json")).unwrap();
        let end = last.end.unwrap();
        assert_eq!(
            (
                end.service_result.as_str(),
                end.stop_intent,
                end.stop_signal
            ),
            ("success", false, None)
        );
        assert_eq!(last.invocation_id, "0f3c9d2e8b7a4c1d9e6f5a4b3c2d1e0f");
        let release: ReleaseFile =
            serde_json::from_str(include_str!("../tests/fixtures/release.json")).unwrap();
        assert_eq!(
            release
                .environment
                .get("DSH_PERMISSION_MODE")
                .map(String::as_str),
            Some("danger-full-access")
        );
    }

    // catches: a start half written without `end: null`, or one that a reader cannot parse back, so a run that
    // never ended could not be told from a record that is missing
    #[test]
    fn start_half_has_a_null_end() {
        let start = LastRun {
            id: "i".to_owned(),
            release_path: OsText::from("/nix/store/r"),
            boot_id: "b".to_owned(),
            started_at: 1,
            invocation_id: "v".to_owned(),
            end: None,
            unrecognized: serde_json::Map::new(),
        };
        let text = String::from_utf8(to_json(&start).unwrap()).unwrap();
        assert!(text.contains("\"end\": null"), "{text}");
        assert_eq!(serde_json::from_str::<LastRun>(&text).unwrap(), start);
        let written = r#"{"id":"i","releasePath":"/r","bootId":"b","startedAt":1,"invocationId":"v","end":null}"#;
        assert!(
            serde_json::from_str::<LastRun>(written)
                .unwrap()
                .end
                .is_none()
        );
    }

    // catches: an end written by an older poststop read as having consumed nothing, the run's totals lost on a
    // round trip, or a record without totals serialized differently from before
    #[test]
    fn run_end_resources_are_optional() {
        let old: LastRun =
            serde_json::from_str(include_str!("../tests/fixtures/last-run.json")).unwrap();
        let mut end = old.end.clone().unwrap();
        assert_eq!(end.resources, None);
        assert!(end.unrecognized.is_empty());
        let text = String::from_utf8(to_json(&old).unwrap()).unwrap();
        assert!(!text.contains("resources"), "{text}");
        end.resources = Some(RunResources {
            cpu_usage_nsec: Some(2_500_000_000),
            memory_peak: Some(73_400_320),
            io_read_bytes: None,
            io_write_bytes: Some(0),
            unrecognized: serde_json::Map::new(),
        });
        let text = serde_json::to_string(&end).unwrap();
        assert!(
            text.contains(r#""resources":{"cpuUsageNSec":2500000000,"memoryPeak":73400320,"ioReadBytes":null,"ioWriteBytes":0}"#),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<RunEnd>(&text).unwrap(), end);
    }

    // catches: a mirrored exit that differs from what a shell reports: a code changed, 128+S wrapping instead of
    // saturating, or an end with no status read as success
    #[test]
    fn shell_status_follows_the_shell() {
        let rows = [
            (Some(0), None, 0),
            (Some(75), None, 75),
            (Some(255), None, 255),
            (Some(256), None, 1),
            (None, Some(15), 143),
            (None, Some(9), 137),
            (None, Some(200), 255),
            (None, Some(-1), 255),
            (None, None, 1),
        ];
        for (exit_code, signal, expected) in rows {
            assert_eq!(
                ChildEnd { exit_code, signal }.shell_status(),
                expected,
                "{exit_code:?} {signal:?}"
            );
        }
    }
}
