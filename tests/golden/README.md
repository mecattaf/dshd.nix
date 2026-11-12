# CLI golden cases

Each directory under `tests/golden/` except `base/` is one run of `dshd`
against fake `systemctl`, `journalctl`, `nix-store` and `xdg-open` under a
private root, with its expected stdout, stderr, exit code and the argv the
fakes recorded. The expectations are the documentation's source of truth:
the README examples are copied from them, and `tests/cli.rs` fails when a
case the README cites by name is gone.

The cases run under Nix: the checks build the release `%RELEASE%` stands
for and set `DSHD_GOLDEN_RELEASE` to it, so every case runs there. A plain
`cargo test` without that variable checks only the shape of each case (its
entries, its `argv` and its numeric `exit`) and says so on stderr; a
`DSHD_GOLDEN_RELEASE` that names no directory is an error.

stdout is a pipe, so a verb prints JSON unless the case passes `--text`;
`read`, `attach` and `help` print text unless it passes `--json`, and
errors follow the same rule. `tests/contract.rs` checks every JSON line of
the expectations against `tests/contract/dshd.schema.json`, and that
contract against `tests/contract/published.schema.json`, the contract as
last published: a change that breaks a reader of the published one fails.
A commit that changes the contract copies `dshd.schema.json` over the
published one once that comparison passed against the old copy, or else
adds a **Breaking:** entry to the CHANGELOG.

## Substitutions

- `%ROOT%`: the case's private root, a fresh directory, canonical (no
  symlink anywhere in its path).
- `%RELEASE%`: a release store path built by `lib.mkRelease` from the stub
  dsh with version `golden`, so its store name is
  `dshd-release-stub-dsh-golden` and its label `stub-dsh-golden`. The
  harness reads it from `DSHD_GOLDEN_RELEASE`; without it the set does not
  run, and the harness says so.

Both are substituted in `argv`, `env`, `links` and in the contents of every
file copied from `base/root` and a case's `root`. In stdout, stderr and the
recorded calls, the value of `%RELEASE%` is replaced first, then the value
of `%ROOT%`.

## The root

Before each case the harness:

1. creates `%ROOT%/{bin,home,work,config,fake/log}`;
2. puts the four fakes from `tests/stub/` into `%ROOT%/bin`, with links to
   the POSIX utilities they need, so `PATH` holds nothing else;
3. copies `base/root/` over `%ROOT%`, then the case's `root/` if present;
4. creates the symlinks listed in `base/links`, then in the case's `links`
   (one `PATH TARGET` per line, `PATH` relative to `%ROOT%`);
5. removes the paths listed in the case's `absent`.

`dshd` then runs with `%ROOT%/work` as its working directory, stdin closed,
and exactly this environment, plus the case's `env` lines (`KEY=VALUE`):

```
PATH=%ROOT%/bin
HOME=%ROOT%/home
DSH_HOME=%ROOT%/dsh
NO_COLOR=1
FAKE_DIR=%ROOT%/fake
```

The base holds five instances under `%ROOT%/state/dshd`, every one created
from `%RELEASE%` in `%ROOT%/work` with the environment above:

| Instance | Transport | Unit | Notes |
|----------|-----------|------|-------|
| `web-1`  | web  | running | label `agency`, pinned port 41234, `web-url` announced, a last run under an earlier boot that `dshd stop` ended |
| `review` | acp  | stopped | |
| `batch`  | none | stopped | |
| `old`    | web  | stopped | release name `default`, absent from `/etc/dshd/releases` |
| `broken` | none | never ran | `instance.json` has the unknown key `colour` |

The profiles `web`, `acp` and `headless` exist in the DSH home, their
`package.json` listing the scoped bundles
`["@deepseek-ai/dsh-base","@deepseek-ai/dsh-<app>"]`, so `run` runs `dsh`
itself only for a profile that is missing, as `run-profile-missing` does.
The sandbox is assumed to have no `/etc/dshd/releases`.

Every case also checks that `%ROOT%/dsh` is the same tree after the run as
before it: the same entries, modes, bytes and link targets. dshd only reads
the DSH home.

## The fakes

Every fake appends its argv, space-joined, as one line to
`$FAKE_DIR/log/<tool>`; `systemctl` logs every verb except the three
queries (`show-environment`, `show`, `list-units`).

`systemctl` exits 1 unless its first argument is `--user`. A unit's state
is the word in `$FAKE_DIR/units/<unit>`: `running`, `dead` (ran, then
stopped), `failed`, `masked`, or `exited N` (a headless task that ended by
itself with status N); a unit without a file has never run.

- `show-environment`: `XDG_STATE_HOME=%ROOT%/state`,
  `XDG_RUNTIME_DIR=%ROOT%/run`, `XDG_CONFIG_HOME=%ROOT%/config` and
  `HOME=%ROOT%/home`, with `%ROOT%` the parent of `$FAKE_DIR`. When
  `$FAKE_DIR/manager-down` exists it prints
  `Failed to connect to bus: No medium found` on stderr and exits 1.
- `show --timestamp=unix -p PROPERTIES UNIT...`: one block per unit, blank
  line between, every property listed. All units are `loaded`.
  - never run: `inactive`/`dead`, `success`, no timestamps;
  - `running`: `active`/`running`, `success`, `MainPID=4242`,
    `ActiveEnterTimestamp=@1794009610`,
    `InvocationID=0123456789abcdef0123456789abcdef`;
  - `dead`: `inactive`/`dead`, `success`, `ActiveEnterTimestamp=@1794009610`,
    `InactiveEnterTimestamp=@1794013210`;
  - `failed`: `failed`/`failed`, `exit-code`, `ExecMainStatus=1`,
    `InactiveEnterTimestamp=@1794013210`;
  - `masked`: `LoadState=masked`, `inactive`/`dead`, no timestamps;
  - `exited 0`: as `dead`; `exited N` otherwise: `failed`/`failed`,
    `exit-code`, `ExecMainStatus=N`, `InactiveEnterTimestamp=@1794013210`;
    both keep `InvocationID=0123456789abcdef0123456789abcdef`;
  - `dshd.target`: `active`, and `not-found` when
    `$FAKE_DIR/template-missing` exists;
  - `WantedBy`: `dshd.target` while the link
    `%ROOT%/config/systemd/user/dshd.target.wants/UNIT` exists, else empty.
- `show -p UnitPath` with no unit: `%ROOT%/config/systemd/user.control`,
  `%ROOT%/run/systemd/user.control`, `%ROOT%/config/systemd/user` and
  `/etc/systemd/user`. When `$FAKE_DIR/unit-path-elsewhere` exists,
  `%ROOT%/elsewhere/systemd/user` replaces the first and third, a directory
  the fake reads no links from.
- `list-units --all --plain --no-legend PATTERN`: one line
  `UNIT LOAD ACTIVE SUB dshd instance` per file in `$FAKE_DIR/units`.
- `start UNIT` and `restart UNIT`: the state becomes `running`. With
  `FAKE_FAIL=start` it becomes `failed`, and the fake prints
  `Job for UNIT failed because the control process exited with error code.`
  on stderr and exits 1. With `FAKE_EXIT=N` it becomes `exited N`, and the
  fake writes the instance's `last-run.json` as the runner's post-stop step
  would: invocation `0123456789abcdef0123456789abcdef`, the end
  `exited`/`N` with the child's exit code N, `serviceResult` `success` for
  0 and `exit-code` otherwise, no stop intent.
- `stop UNIT`: the state becomes `dead`. With `FAKE_FAIL=stop` the state
  stays, and the fake prints `Failed to stop UNIT: Access denied` on stderr
  and exits 1.
- `reset-failed UNIT`: `failed`, and `exited N` for N other than 0, become
  `dead`.
- `daemon-reload` does nothing. With `FAKE_FAIL=daemon-reload` it prints
  `Failed to reload daemon: Access denied` on stderr and exits 1.
- `disable UNIT`: removes that link.

`journalctl` prints a journal: with `_SYSTEMD_INVOCATION_ID=ID`,
`$FAKE_DIR/invocations/ID`, else `$FAKE_DIR/journal`; a missing file is
empty. `-n +K` prints the oldest K lines and `-n K` the newest K. `-f`
with `--no-tail` prints the whole journal and with `-n 0` none of it,
stopping after the runner's last line (`dshd-run: run ended`) or at the
end of the file, since it has no later lines to wait for. `nix-store
--option substitute false --realise PATH --add-root LINK` points `LINK` at
`PATH` and prints `LINK`. `xdg-open` only logs.

## A case

| File | Meaning |
|------|---------|
| `argv` | the arguments after `dshd`, one per line (required) |
| `exit` | the exit code (required) |
| `stdout`, `stderr` | the expected output; an absent file means empty |
| `env` | `KEY=VALUE` lines added to the environment |
| `root/` | files copied over the base |
| `links` | more symlinks, as in `base/links` |
| `absent` | paths under `%ROOT%` removed before the run |
| `calls/<tool>` | the expected log of that fake; an empty file means the tool was not called |
| `gone` | paths under `%ROOT%` that must not exist after the run |

Whether linger is on depends on the host, not on dshd, so it is taken out
before the comparison: stderr lines starting `dshd: warning: linger is off`
and stdout lines starting `  linger: ` are dropped, and in a JSON line the
`linger` value becomes a token and the warnings starting `linger is off`
are dropped. JSON lines are compared as values, not as text.

An `env` key ending `_CANARY` is a secret of the creating shell. Its value
must not reach stdout, stderr or any fake's argv; under `%ROOT%` it may be
only in the `instance.json` of an instance the case created, with mode
0600, and it must be there.

`run-release-invalid` and `enable-systemctl-failed` pin those error
kinds against failing tools; `enable-systemctl-failed` and
`enable-unloaded` also check that a wants link the manager did not load is
removed again. `rm-enabled` checks removal of autostart membership,
`rm-inconsistent` that the remedy `get` prints for a record a newer dshd
wrote removes it, and `tui-refused` checks an unsupported terminal
profile.
`stop-invocation-other` is the compare-and-stop fence: `web-1` runs
another invocation than the one its recorded last run names, and a stop
that names the recorded one stops nothing and writes no stop intent; the
other `stop-invocation-*` cases pin the stopped, already-stopped and
refused outcomes.
`attached-elsewhere` is exercised by the real relay in
`tests/relay/handover.rs`; release updates run in `checks/switch.nix`.
The complete error kind/exit table is in `src/error/unit_tests.rs`.
An `_SCRUBBED` environment value must reach neither output nor any file
under the case root, including the instance record.

`calls/systemctl-queries` optionally pins read-only calls, including their
batching. Other tool traces are always compared; an absent expectation
means no call is permitted. Set `DSHD_GOLDEN_ACTUAL` to a scratch directory
to save normalized binary results for review. It never updates expectations
or bypasses assertions.
