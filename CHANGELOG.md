# Major Changes in dshd.nix

## Unreleased

- `dshd`, a command that keeps native `dsh` processes running as
  `dshd@NAME.service` units in the invoking user's systemd user manager.
  Its fifteen verbs are `run`, `start`, `stop`, `restart`, `update`, `ls`,
  `get`, `read`, `wait`, `attach`, `label`, `enable`, `disable`, `rm` and
  `help`; `dshd` alone is `dshd ls`, and `dshd --skill` prints a guide for
  agents. `run` creates instances and requires `--` before DSH's
  arguments; `start` only starts existing ones.
- Output is JSON when stdout is not a terminal or with `--json`, and text
  on a terminal or with `--text`; `read` and `attach` print text unless
  `--json` is given. Every mutating verb reports an `outcome` and its
  `warnings`, `ls` prints one object with `instances` and `linger`, and
  errors carry a stable `kind` and a `hint`.
- Exit codes 0 to 7 by error kind, 124 when `run` or `wait` checks in while
  the instance still runs, and in text mode `dsh`'s own exit code, or
  128+S for a signal, from a followed run.
- A headless task runs once: a foreground `run` prints its output and
  exits as `dsh` did, `wait` and `read` pick it up later, and only
  `start --rerun` submits the prompt again.
- Each run records how it ended (`stopped`, `stopped-without-intent`,
  `exited`, `failed`, `interrupted`), from the stop intent dshd writes
  before it stops an instance; a unit the user masked shows as `masked`.
- Native accounting counters under `get.resources`, with lane history
  available directly from journald lifecycle records.
- Labels on instances, set by `run --label` and `label`.
- `run` refuses a `--patch` launcher flag with an empty value, which every
  start of the instance would fail on.
- `dshd-run`, the per-release runner: spawns `dsh` with the recorded argv,
  working directory and environment, reports readiness to systemd, keeps a
  web instance's origin and default port, and relays an ACP instance's stdio to one
  controller at a time, holding sessions and permission requests between
  controllers.
- Profiles stay DSH's own: `dsh` gets the argv as typed and composes the
  live profile of its DSH home at every start, and dshd writes nothing in
  that home. dshd reads the profile's bundles for the transport, records
  it at creation and fails a start whose profile now selects another; a
  shipped profile that does not exist yet is created by `dsh` itself.
- The NixOS module `dshd` (`enable`, `package`, `releases`,
  `defaultRelease`, `startupTimeout`): one static `dshd@.service` template
  that no switch restarts or stops, `dshd.target`, and the release
  registry under `/etc/dshd/releases`.
- `lib.mkRelease`, which pins a DSH build, the runner and the release's
  environment defaults in one store path; `DSH_PERMISSION_MODE` defaults
  to `danger-full-access`.
- Frozen names: the `dshd@NAME.service` unit and `dshd.target`, the files
  under `%S/dshd/NAME` and `%t/dshd/NAME`, `/etc/dshd/releases/NAME` and
  the run start and end journal lines.
- Checks: cargo tests with the golden CLI cases and the contract tests,
  clippy, rustfmt, nixfmt, deadnix, the module's rendered unit, public-tree
  hygiene with the licence and attribution, and NixOS VM tests of the
  lifecycle, the transports, switches and reboots.
- Licensed under Apache-2.0; `NOTICE` holds the copyright and the
  attribution of adapted code.
- **Breaking:** the per-instance profile snapshot is gone. Removed: the
  `composition` field of `instance.json`; the `exposed`, `linked` and
  `edited` fields of `profile` in `get` and the other reports; the error
  kind `profile-link-exists`; the `profile/` and `gcroots/` entries of
  `%S/dshd/NAME`; and the frozen `<profile>@NAME` profile link. `args` in
  `instance.json` and in the reports is the argv as typed, without a
  rewritten `--profile`. A record with `composition` is refused as
  unknown: remove the instance with `dshd rm NAME` and run it again.
