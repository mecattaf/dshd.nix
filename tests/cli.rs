//! The golden CLI cases under `tests/golden`: the `dshd` binary against the
//! fake `systemctl`, `journalctl`, `nix-store` and `xdg-open` of
//! `tests/stub`, each case in a private root. `tests/golden/README.md` is
//! the case format and the fakes' contract; this harness follows it. The
//! `dshd` examples of the README and of the agent guide must parse.
//!
//! `%RELEASE%` is a release store path built from the stub dsh, named by
//! `DSHD_GOLDEN_RELEASE`. Without it the cases are only checked for their
//! shape, and the harness says so on stderr. In every case the DSH home is
//! the same tree after `dshd` ran as before: dshd never writes there.

mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output, Stdio};

use serde_json::Value;

use tree::tree;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The cases, one directory each, and the shared base.
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
/// The fakes, as `<tool>.sh`.
const STUB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/stub");
/// The binary under test.
const DSHD: &str = env!("CARGO_BIN_EXE_dshd");
/// Names the release store path `%RELEASE%` stands for.
const RELEASE_VARIABLE: &str = "DSHD_GOLDEN_RELEASE";
/// The directory every case starts from.
const BASE: &str = "base";
/// The DSH home under the case root, which `dshd` only reads.
const DSH_HOME: &str = "dsh";
/// The fakes put on the case's `PATH`.
const FAKES: [&str; 4] = ["systemctl", "journalctl", "nix-store", "xdg-open"];
/// The utilities the fakes run, linked from the test's own `PATH`.
const UTILITIES: [&str; 8] = ["cat", "mkdir", "ln", "rm", "head", "tail", "sleep", "sed"];
/// What a case directory may hold.
const CASE_ENTRIES: [&str; 10] = [
    "argv", "exit", "stdout", "stderr", "env", "root", "links", "absent", "calls", "gone",
];
const ROOT_TOKEN: &str = "%ROOT%";
const RELEASE_TOKEN: &str = "%RELEASE%";
/// Lines that depend on the host's linger setting, not on dshd.
const HOST_STDERR: &str = "dshd: warning: linger is off";
const HOST_STDOUT: &str = "  linger: ";
/// The same in a JSON line: the warning, and the value `linger` becomes.
const HOST_WARNING: &str = "linger is off";
const LINGER_TOKEN: &str = "%LINGER%";
/// Cases the README and the contract test cite by name.
const NAMED: &[&str] = &[
    "run-twice-same",
    "run-different-recipe",
    "get-json",
    "attach-acp-on-web",
    "attach-stopped",
    "read-exec",
];
/// An `env` key ending so is a secret dshd may hold only in the record.
const CANARY_SUFFIX: &str = "_CANARY";
/// Mode of the one file a canary may be in.
const RECORD_MODE: u32 = 0o600;

// catches: a changed output, outcome, error kind, exit code or tool argv in a golden case, a malformed or
// renamed cited case, a creating shell's secret leaking past the record, a release variable naming nothing,
// and any write dshd makes in the DSH home
#[test]
fn golden_cases_hold_without_drift() -> TestResult {
    let cases = case_names()?;
    assert!(!cases.is_empty(), "no golden cases under {GOLDEN}");
    let mut failures: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            check_shape(case)
                .err()
                .map(|error| format!("{case}: {error}"))
        })
        .collect();
    failures.extend(
        NAMED
            .iter()
            .filter(|named| !cases.iter().any(|case| case == *named))
            .map(|named| format!("{named}: cited, but no such case")),
    );
    assert!(
        failures.is_empty(),
        "malformed golden cases:\n{}",
        failures.join("\n")
    );

    let Some(release) = std::env::var_os(RELEASE_VARIABLE) else {
        let count = cases.len();
        writeln!(
            io::stderr(),
            "dshd golden cases: {RELEASE_VARIABLE} is not set, so the {count} cases were checked for shape only"
        )?;
        return Ok(());
    };
    let release = PathBuf::from(release);
    if !release.is_dir() {
        return Err(format!(
            "{RELEASE_VARIABLE}={} names no directory",
            release.display()
        )
        .into());
    }
    for case in &cases {
        match run_case(case, &release) {
            Ok(differences) => failures.extend(
                differences
                    .into_iter()
                    .map(|difference| format!("{case}: {difference}")),
            ),
            Err(error) => failures.push(format!("{case}: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "golden cases differ from dshd:\n\n{}",
        failures.join("\n\n")
    );
    Ok(())
}

/// Every case directory, sorted, without the base.
fn case_names() -> TestResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(GOLDEN)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("a case name that is not UTF-8: {}", name.to_string_lossy()))?;
        if entry.file_type()?.is_dir() && name != BASE {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// A case holds only known entries, an `argv` and a numeric `exit`.
fn check_shape(case: &str) -> TestResult {
    let dir = Path::new(GOLDEN).join(case);
    for entry in fs::read_dir(&dir)? {
        let name = entry?.file_name();
        if !name
            .to_str()
            .is_some_and(|name| CASE_ENTRIES.contains(&name))
        {
            return Err(format!("unknown entry {}", name.to_string_lossy()).into());
        }
    }
    fs::read_to_string(dir.join("argv")).map_err(|error| format!("argv: {error}"))?;
    let exit = fs::read_to_string(dir.join("exit")).map_err(|error| format!("exit: {error}"))?;
    exit.trim()
        .parse::<i32>()
        .map_err(|error| format!("exit {exit:?}: {error}"))?;
    Ok(())
}

/// Runs one case in a fresh root, removed afterwards; the differences.
fn run_case(case: &str, release: &Path) -> TestResult<Vec<String>> {
    let scratch = std::env::temp_dir().join(format!("dshd-golden-{case}-{}", process::id()));
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    fs::create_dir_all(&scratch)?;
    let root = scratch.canonicalize()?;
    let case = Case {
        dir: Path::new(GOLDEN).join(case),
        root: text_of(&root)?.to_owned(),
        release: text_of(release)?.to_owned(),
    };
    let differences = case.prepare().and_then(|()| case.run());
    fs::remove_dir_all(&root)?;
    differences
}

/// One case: its directory, its root and the release, as text.
struct Case {
    dir: PathBuf,
    root: String,
    release: String,
}

impl Case {
    fn under_root(&self, relative: &str) -> PathBuf {
        Path::new(&self.root).join(relative)
    }

    /// The tokens replaced by what they stand for.
    fn substitute(&self, text: &str) -> String {
        text.replace(RELEASE_TOKEN, &self.release)
            .replace(ROOT_TOKEN, &self.root)
    }

    /// The release, then the root, replaced by their tokens.
    fn normalize(&self, text: &str) -> String {
        text.replace(&self.release, RELEASE_TOKEN)
            .replace(&self.root, ROOT_TOKEN)
    }

    /// A file of the case; `None` when the case has none.
    fn read(&self, file: &str) -> TestResult<Option<String>> {
        match fs::read_to_string(self.dir.join(file)) {
            Ok(text) => Ok(Some(text)),
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{file}: {error}").into()),
        }
    }

    /// The root as the README lays it out, before `dshd` runs.
    fn prepare(&self) -> TestResult {
        for dir in ["bin", "home", "work", "config", "fake/log"] {
            fs::create_dir_all(self.under_root(dir))?;
        }
        for fake in FAKES {
            symlink(
                Path::new(STUB).join(format!("{fake}.sh")),
                self.under_root(&format!("bin/{fake}")),
            )?;
        }
        for utility in UTILITIES {
            symlink(
                on_test_path(utility)?,
                self.under_root(&format!("bin/{utility}")),
            )?;
        }
        let base = Path::new(GOLDEN).join(BASE);
        self.overlay(&base.join("root"), Path::new(&self.root))?;
        if self.dir.join("root").is_dir() {
            self.overlay(&self.dir.join("root"), Path::new(&self.root))?;
        }
        self.link(&fs::read_to_string(base.join("links"))?)?;
        if let Some(links) = self.read("links")? {
            self.link(&links)?;
        }
        for relative in self
            .read("absent")?
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
        {
            let path = self.under_root(relative);
            if fs::symlink_metadata(&path)?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Copies a tree over the root, substituting the tokens in every file.
    fn overlay(&self, from: &Path, to: &Path) -> TestResult {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.overlay(&entry.path(), &target)?;
            } else if kind.is_file() {
                match String::from_utf8(fs::read(entry.path())?) {
                    Ok(text) => fs::write(&target, self.substitute(&text))?,
                    Err(binary) => fs::write(&target, binary.into_bytes())?,
                }
            } else {
                return Err(format!(
                    "{} is neither a file nor a directory",
                    entry.path().display()
                )
                .into());
            }
        }
        Ok(())
    }

    /// Creates the `PATH TARGET` links of a `links` file, replacing any
    /// link already there.
    fn link(&self, links: &str) -> TestResult {
        for line in links.lines().filter(|line| !line.trim().is_empty()) {
            let (relative, target) = line
                .split_once(' ')
                .ok_or_else(|| format!("a link without a target: {line:?}"))?;
            let link = self.under_root(relative);
            if let Some(parent) = link.parent() {
                fs::create_dir_all(parent)?;
            }
            if fs::symlink_metadata(&link).is_ok() {
                fs::remove_file(&link)?;
            }
            symlink(self.substitute(target.trim()), &link)?;
        }
        Ok(())
    }

    /// Runs `dshd` and lists how its results differ from the case's.
    fn run(&self) -> TestResult<Vec<String>> {
        let argv: Vec<String> = self
            .read("argv")?
            .ok_or("no argv")?
            .lines()
            .map(|arg| self.substitute(arg))
            .collect();
        let home = self.under_root(DSH_HOME);
        let home_before = tree(&home)?;
        let output = self.dshd(&argv)?;
        self.record_actual(&output)?;
        let mut differences = Vec::new();
        let home_after = tree(&home)?;
        let paths: BTreeSet<&PathBuf> = home_before.keys().chain(home_after.keys()).collect();
        for path in paths {
            let (was, is) = (home_before.get(path), home_after.get(path));
            if was != is {
                differences.push(format!(
                    "dshd changed {} in the DSH home: {} became {}",
                    self.normalize(&path.display().to_string()),
                    was.map_or("nothing", String::as_str),
                    is.map_or("nothing", String::as_str)
                ));
            }
        }
        let exit: i32 = self.read("exit")?.ok_or("no exit")?.trim().parse()?;
        if output.status.code() != Some(exit) {
            differences.push(format!(
                "exit status {:?}, expected {exit}",
                output.status.code()
            ));
        }
        let stdout = self.normalize(&String::from_utf8_lossy(&output.stdout));
        compare(
            "stdout",
            &self.read("stdout")?.unwrap_or_default(),
            &stdout,
            Some(HOST_STDOUT),
            &mut differences,
        );
        let stderr = self.normalize(&String::from_utf8_lossy(&output.stderr));
        compare(
            "stderr",
            &self.read("stderr")?.unwrap_or_default(),
            &stderr,
            Some(HOST_STDERR),
            &mut differences,
        );
        for tool in FAKES
            .into_iter()
            .chain(std::iter::once("systemctl-queries"))
        {
            if tool == "systemctl-queries" && !self.dir.join("calls/systemctl-queries").is_file() {
                continue;
            }
            let expected = self.read(&format!("calls/{tool}"))?.unwrap_or_default();
            let recorded = match fs::read_to_string(self.under_root(&format!("fake/log/{tool}"))) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
                recorded => self.normalize(&recorded?),
            };
            compare(
                &format!("calls of {tool}"),
                &expected,
                &recorded,
                None,
                &mut differences,
            );
        }
        for relative in self
            .read("gone")?
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
        {
            if fs::symlink_metadata(self.under_root(relative)).is_ok() {
                differences.push(format!("{relative} still exists"));
            }
        }
        self.check_canaries(&output, &mut differences)?;
        Ok(differences)
    }

    /// Saves normalized binary results for deliberate snapshot review. This
    /// never updates expectations or bypasses any assertion.
    fn record_actual(&self, output: &Output) -> TestResult {
        let Some(destination) = std::env::var_os("DSHD_GOLDEN_ACTUAL") else {
            return Ok(());
        };
        let name = self.dir.file_name().ok_or("case directory has no name")?;
        let dir = PathBuf::from(destination).join(name);
        fs::create_dir_all(dir.join("calls"))?;
        for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            fs::write(
                dir.join(stream),
                self.normalize(&String::from_utf8_lossy(bytes)),
            )?;
        }
        fs::write(
            dir.join("exit"),
            format!("{}\n", output.status.code().ok_or("dshd died by signal")?),
        )?;
        for tool in FAKES
            .into_iter()
            .chain(std::iter::once("systemctl-queries"))
        {
            let file = self.under_root(&format!("fake/log/{tool}"));
            if file.is_file() {
                fs::write(
                    dir.join("calls").join(tool),
                    self.normalize(&fs::read_to_string(file)?),
                )?;
            }
        }
        Ok(())
    }

    /// Every `*_CANARY` value of the case's `env` reaches no output and no
    /// fake's argv: under the root it is only in the `instance.json` of an
    /// instance the case created, with mode 0600.
    fn check_canaries(&self, output: &Output, differences: &mut Vec<String>) -> TestResult {
        let env = self.read("env")?.unwrap_or_default();
        let canaries = env
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(key, _)| key.ends_with(CANARY_SUFFIX));
        for (key, secret) in env
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(key, _)| key.ends_with("_SCRUBBED"))
        {
            if [&output.stdout, &output.stderr]
                .iter()
                .any(|stream| String::from_utf8_lossy(stream).contains(secret))
            {
                differences.push(format!("{key} reached output"));
            }
            let mut holders = Vec::new();
            files_holding(Path::new(&self.root), secret.as_bytes(), &mut holders)?;
            if !holders.is_empty() {
                differences.push(format!("{key} was retained at rest"));
            }
        }
        for (key, canary) in canaries {
            for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
                if String::from_utf8_lossy(bytes).contains(canary) {
                    differences.push(format!("{key} reached {stream}"));
                }
            }
            let mut holders = Vec::new();
            files_holding(Path::new(&self.root), canary.as_bytes(), &mut holders)?;
            let mut records = 0;
            for path in holders {
                let relative = path
                    .strip_prefix(&self.root)?
                    .to_string_lossy()
                    .into_owned();
                let created = relative
                    .strip_prefix("state/dshd/")
                    .and_then(|rest| rest.strip_suffix("/instance.json"))
                    .is_some_and(|name| {
                        !name.contains('/')
                            && !Path::new(GOLDEN)
                                .join(BASE)
                                .join("root/state/dshd")
                                .join(name)
                                .exists()
                    });
                if !created {
                    differences.push(format!("{key} reached {relative}"));
                } else if fs::metadata(&path)?.mode() & 0o777 != RECORD_MODE {
                    differences.push(format!(
                        "{relative} holds {key} with a mode other than {RECORD_MODE:o}"
                    ));
                } else {
                    records += 1;
                }
            }
            if records == 0 {
                differences.push(format!("{key} is in no instance.json the case created"));
            }
        }
        Ok(())
    }

    /// `dshd ARGV` in `%ROOT%/work`, stdin closed, with exactly the base
    /// environment and the case's `env` lines.
    fn dshd(&self, argv: &[String]) -> TestResult<Output> {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("PATH".to_owned(), format!("{}/bin", self.root));
        env.insert("HOME".to_owned(), format!("{}/home", self.root));
        env.insert("DSH_HOME".to_owned(), format!("{}/dsh", self.root));
        env.insert("NO_COLOR".to_owned(), "1".to_owned());
        env.insert("FAKE_DIR".to_owned(), format!("{}/fake", self.root));
        for line in self
            .read("env")?
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
        {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("an env line without '=': {line:?}"))?;
            env.insert(key.to_owned(), self.substitute(value));
        }
        let output = Command::new(DSHD)
            .args(argv)
            .env_clear()
            .envs(&env)
            .current_dir(self.under_root("work"))
            .stdin(Stdio::null())
            .output()?;
        Ok(output)
    }
}

/// Records a difference between expected and actual text, after dropping
/// the host-dependent lines starting with `host` from both and making the
/// JSON lines host-free.
fn compare(
    label: &str,
    expected: &str,
    actual: &str,
    host: Option<&str>,
    differences: &mut Vec<String>,
) {
    let keep = |text: &str| -> String {
        text.split_inclusive('\n')
            .filter(|line| host.is_none_or(|host| !line.starts_with(host)))
            .map(|line| host_free(line).unwrap_or_else(|| line.to_owned()))
            .collect()
    };
    let (expected, actual) = (keep(expected), keep(actual));
    if expected != actual {
        differences.push(format!(
            "{label} differs\n--- expected\n{expected}--- actual\n{actual}"
        ));
    }
}

/// A JSON object line with `linger` replaced by a token and the warnings
/// about it dropped, keys sorted; `None` for any other line.
fn host_free(line: &str) -> Option<String> {
    if !line.starts_with('{') {
        return None;
    }
    let mut value: Value = serde_json::from_str(line.trim_end()).ok()?;
    let object = value.as_object_mut()?;
    if let Some(linger) = object.get_mut("linger") {
        *linger = Value::String(LINGER_TOKEN.to_owned());
    }
    if let Some(Value::Array(warnings)) = object.get_mut("warnings") {
        warnings.retain(|warning| {
            !warning
                .as_str()
                .is_some_and(|warning| warning.starts_with(HOST_WARNING))
        });
    }
    Some(format!("{value}\n"))
}

/// The regular files under `dir` that contain `needle`; links are not
/// followed.
fn files_holding(dir: &Path, needle: &[u8], found: &mut Vec<PathBuf>) -> TestResult {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files_holding(&entry.path(), needle, found)?;
        } else if kind.is_file()
            && fs::read(entry.path())?
                .windows(needle.len())
                .any(|window| window == needle)
        {
            found.push(entry.path());
        }
    }
    Ok(())
}

/// A utility from the test's own `PATH`.
fn on_test_path(name: &str) -> TestResult<PathBuf> {
    let path = std::env::var_os("PATH").ok_or("PATH is not set")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("{name} is not on PATH").into())
}

fn text_of(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| format!("a path that is not UTF-8: {}", path.display()).into())
}
