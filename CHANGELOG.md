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
- `dshd-run`, the per-release runner: spawns `dsh` with the recorded argv,
  working directory and environment, reports readiness to systemd, keeps a
  web instance's origin and default port, and relays an ACP instance's stdio to one
  controller at a time, holding sessions and permission requests between
  controllers.
- Per-instance profile copies exposed to DSH as `<profile>@NAME`, so a
  running instance never sees an edit to its source profile.
- The NixOS module `dshd` (`enable`, `package`, `releases`,
  `defaultRelease`, `startupTimeout`): one static `dshd@.service` template
  that no switch restarts or stops, `dshd.target`, and the release
  registry under `/etc/dshd/releases`.
- `lib.mkRelease`, which pins a DSH build, the runner and the release's
  environment defaults in one store path; `DSH_PERMISSION_MODE` defaults
  to `danger-full-access`.
- Frozen names: the `dshd@NAME.service` unit and `dshd.target`, the files
  under `%S/dshd/NAME` and `%t/dshd/NAME`, `/etc/dshd/releases/NAME`, the
  `<profile>@NAME` profile link and the run start and end journal lines.
- Checks: cargo tests with the golden CLI cases and the contract tests,
  clippy, rustfmt, nixfmt, deadnix, the module's rendered unit, public-tree
  hygiene with the licence and attribution, and NixOS VM tests of the
  lifecycle, the transports, switches and reboots.
- Licensed under Apache-2.0; `NOTICE` holds the copyright and the
  attribution of adapted code.
