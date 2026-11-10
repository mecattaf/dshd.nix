//! The on-disk records: `instance.json`, `last-run.json` and the release's
//! `release.json`. Objects with camelCase keys, no version marker. Unknown
//! keys are kept in `unrecognized` so a reader that only reports can pass
//! them through; a reader that acts (the runner, `update`, `remove`) refuses
//! a record whose `unrecognized` map is not empty.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    /// Native `dsh` arguments as typed. Web defaults are injected
    /// independently at each start.
    pub args: Vec<OsText>,
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
