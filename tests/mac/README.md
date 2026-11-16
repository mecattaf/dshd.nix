# The Mac test kit

`acceptance.sh` runs the acceptance of dshd on a Mac that has no Nix in
dshd's way: an arm64 `dshd` and `dshd-run`, a stock DSH, launchd. Everything
it makes lives in one temporary prefix, `/tmp/dshd-zzt.XXXXXX`, and its
instances are named `zzt-*` (labels `dshd.zzt-*`), so nothing touches the
user's own jobs. It ends with a clean-up that it checks, and a plain table:

```
STEP  RESULT CHECK                                                DETAIL
1     PASS   bin/dshd links only /usr/lib and /System             2 libraries, by otool
3     PASS   get: unit gui/501/dshd.zzt-web                       gui/501/dshd.zzt-web
...
43 passed, 0 failed, 3 skipped, 8 facts
```

It needs only what macOS ships: `/bin/bash` 3.2, `launchctl`, `codesign`,
`ps`, `lsof`, and `jq` or `osascript` for JSON. It runs `otool` only when
the command line tools are installed, since the `/usr/bin/otool` shim would
otherwise offer to install them.

## Getting the binaries onto the Mac

The `macos` workflow (`.github/workflows/macos.yml`) builds every pushed
branch on an Apple silicon runner, runs clippy and the unit tests there,
checks that both binaries link only `/usr/lib` and `/System`, and uploads
`dshd-aarch64-apple-darwin-COMMIT.tar.gz`:

```
gh run download RUN -R OWNER/REPO -n dshd-aarch64-apple-darwin -D artifact
tar -xzf artifact/dshd-aarch64-apple-darwin-*.tar.gz
```

The tarball holds `bin/dshd`, `libexec/dshd/dshd-run`, `SHA256SUMS` and
`BUILDINFO`. Copy the unpacked directory to the Mac with `scp`, so it
carries no quarantine attribute. The binaries are signed ad hoc by the
linker, which is what arm64 macOS needs to run them; they have no
Developer ID signature and are not notarized.

## A session on the Mac

Every check one session can make, then the clean-up:

```
bash acceptance.sh run --dshd DIR/bin/dshd --dsh PATH/TO/dsh
```

`--dsh` is the upstream DSH (or `tests/stub/dsh.sh` before a web chat is
needed). It is used where it is and never copied or changed. By default
DSH runs with `DSH_HOME` inside the temporary prefix; `--own-dsh-home` lets
it use its own. `--path DIR` adds a directory DSH needs, such as its
interpreter's, after the system directories.

Two checks need a person between two halves:

```
bash acceptance.sh hold --for disconnect --dshd DIR/bin/dshd --dsh PATH/TO/dsh
#   close the Terminal window or SSH session, open a new one, then:
bash acceptance.sh resume /tmp/dshd-zzt.XXXXXX

bash acceptance.sh hold --for logout --dshd DIR/bin/dshd --dsh PATH/TO/dsh
#   log out at the screen and in again, then:
bash acceptance.sh resume /tmp/dshd-zzt.XXXXXX
```

Step 8 (no GUI login session) runs instead of steps 3 to 7 when the kit
finds no `gui/UID` domain: over SSH while nobody is logged in at the screen.

After an interrupted run, or a hold that will not be resumed:

```
bash acceptance.sh clean /tmp/dshd-zzt.XXXXXX    # or: clean --prefix zzt
```

`--captures DIR` keeps `launchctl print` text and `dshd get` JSON for the
parser fixtures, with the user name, home, host name and web tokens
replaced. `--capture-domain` adds `print gui/UID` and `print-disabled`,
which name every job of the user: read them before they go anywhere.

## The steps

| step | checks |
|---|---|
| 0 | macOS on arm64; the GUI domain; nothing of the prefix left from before |
| 1 | both binaries: arm64 Mach-O, a valid signature, only system libraries, no Nix store path; `dshd --version`; no `nix` on dshd's `PATH`; DSH not from Nix |
| 2 | `release add`: one read-only entry under `~/.local/state/dshd/.releases`, its `bin/dsh` the given DSH |
| 3 | `run -- web`: state `running`, unit `gui/UID/dshd.zzt-web`, a web URL, launchd holds the job, the plist has no schedule, `KeepAlive` or environment key, not enabled |
| 4 | the runner's parent is launchd and it has no terminal; with a hold, the same invocation after the session closed |
| 5 | `stop`: launchd no longer holds the job, `lastRun.ending` `stopped`; `start`: a new invocation |
| 6 | DSH killed with SIGKILL: running again under a new invocation, the job's pid kept; 6b: a killed runner is not reported running |
| 7 | with a logout hold: `enable` writes the LaunchAgents copy; after logout and login, running under a new invocation; `disable` removes the copy |
| 8 | without a GUI login: exit 7 and the login hint |
| 9 | `disable`, `stop`, `rm`, `release rm`, `release prune` through dshd; a sweep of whatever dshd left, reported as a failure; nothing of the kit remains |
| 10 | with `--captures`: an ended job and the domain text |

The kit uses `launchctl print`, `list` and, in its sweep only, `bootout` of
its own labels; never `kickstart`, `load`, `enable` or `disable`.

## The kit's own test

`selftest.sh` runs the kit on Linux against `fake/dshd`, the stub DSH and
`fake/macos-tools.sh` (stand-ins for `launchctl`, `uname`, `codesign` and the
other macOS commands), in a scratch home, through ten scenarios: the whole
run, both holds, no GUI session, a `KeepAlive` key, a stop that leaks DSH,
the skeleton where every launchd verb is unbuilt, a library in the Nix
store, leftovers, and a hold given up. It proves the kit's verdicts and its
clean-up, not dshd's behaviour on a Mac.
