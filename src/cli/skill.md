# dshd for agents

dshd keeps native DSH sessions running as systemd user units, one unit
`dshd@NAME.service` per instance. It outlives your shell, your SSH session
and your own process. Everything below is stable: verbs, JSON fields, error
kinds and exit codes are only ever added to.

Output: when stdout is not a terminal (your case), results print as one
JSON object per line and errors as one JSON object on stderr. `--text`
forces the human form; `read`, `attach`, `help` and a headless `run` in
the foreground print text unless you pass `--json`. Flags come before
`--`; everything after `--` goes to dsh byte for byte.

## Start a session

Pick the profile by what you need; the profile's bundles decide the
transport.

| You need | Command | Transport |
|----------|---------|-----------|
| a browser UI a human opens | `dshd run --name web-1 -- web` | web |
| an ACP agent you drive over stdio | `dshd run --name review -- acp` | acp |
| one task, its output, its exit code | `dshd run --name batch -- headless "summarise the open issues"` | none |
| a headless task you check on later | `dshd run --bg --name batch -- headless "summarise the open issues"` | none |

- Without `--name`, dshd picks `PROFILE-K`; read it from the `name` field.
- `--cwd DIR` sets the working directory (default: yours), `--release R`
  pins a release, `--label KEY=VALUE` attaches metadata (repeatable).
- The environment of the shell that runs `run` is captured once, at
  creation, and every later start reuses it. Values never appear in any
  output; `get` lists the names only.
- Running `dshd run --name web-1 -- web` again with the same directory,
  arguments and profile is safe: it reports `already-running` or starts
  the stopped instance. Labels given again are applied. A different recipe
  under the same name is refused with `exists-different-recipe`.
- SDK profiles and terminal apps are refused with `unsupported-profile`:
  they need the client or the terminal that starts them.

## Wait for a headless task

A headless `run` waits in the foreground until the task ends. It prints
dsh's output and exits as dsh did, even when piped; after `--timeout`
seconds (default 180) it exits 124 and says on stderr how to pick the task
up again. The task keeps running either way.

```
$ dshd run --name batch --timeout 600 -- headless "run the test suite and fix what fails"
$ dshd wait batch
$ dshd wait --timeout 900 batch
$ dshd read batch
```

- Text mode (`run` without `--json`; `wait` with `--text` or on a
  terminal): they exit with dsh's own exit code, or 128+S when signal S
  ended it, or 124 at the check-in.
- JSON mode (`run --json`; `wait` when piped or with `--json`): they exit 0
  and print an outcome `ended` or `still-running` with `exitCode`, `signal`
  and `reason`. Branch on `outcome` and `exitCode`, not on the process
  status; `read` has the output.

## Read what a session printed

```
$ dshd read batch
$ dshd read -n 50 batch
$ dshd read --follow batch
$ dshd read --json batch
```

`read` prints the journal of the current run, or of the last one when the
instance is stopped. If the lines do not reach back to the start of the
run, stderr says `[dshd] incomplete: older output may be missing`; widen
with `-n`. For an acp instance the journal holds dsh's stderr only, since
its stdout is the ACP channel. `read --json` gives
`{"name","invocationId","complete","streams","lines"}`.

## Connect

```
$ dshd attach web-1
$ dshd attach --print-url web-1
$ dshd attach review
$ dshd attach --takeover review
```

- web: `attach` prints the origin. Only `dshd attach --print-url web-1`
  prints the full URL with its token; treat that output as a secret.
- acp: `attach` runs the ACP client on your stdin and stdout. The relay has
  one controller. A second `attach` fails with `attached-elsewhere`;
  `dshd attach --takeover review` replaces the controller.
- A headless instance cannot be attached (`unsupported-transport`); use
  `dshd read --follow batch`.

## Inspect and manage

```
$ dshd ls
$ dshd get web-1
$ dshd stop web-1
$ dshd start web-1
```

Other verbs: `dshd restart web-1`, `dshd update -R web-1` (follow the
release name), `dshd label web-1 owner=ci` (an empty value removes a
label), `dshd enable web-1` and `dshd disable web-1` (start with the user
manager), `dshd rm web-1` (delete a stopped instance), `dshd help run`.

`dshd get --json NAME` includes native CPU, memory, tasks and IO counters
under `resources`; unavailable counters are `null`. For lane history use
`journalctl --user -u dshd@NAME.service -o json`; lifecycle records share
`USER_INVOCATION_ID`, with `MESSAGE_ID`, `UNIT_RESULT`, `N_RESTARTS` and
resource totals. Use `-u 'dshd@*.service'` for every lane.

## Traps

- `stop` ends every process of the instance, tool children started with
  `nohup` or `&` included. Nothing a session started survives its stop.
- `run` never runs a headless task twice. Once it ran, `start` refuses with
  `task-already-ran`; `dshd start --rerun batch` submits the recorded prompt
  again, on purpose. `restart` refuses a headless instance for the same
  reason.
- ACP has one controller: `--takeover` disconnects whoever was attached.
- Only `attach --print-url` prints the web token. `get`, `ls` and every
  outcome redact it.
- A headless instance has no transport: no `attach`, no `enable`.
- `stop`, `restart` or `update -R` run from inside the instance end your own
  process with it; systemd still completes the action. Check from outside
  with `dshd get web-1`.
- A masked unit (`systemctl --user mask`) is never unmasked by dshd; `start`
  refuses with `unit-masked` and names the command.
- The profile is DSH's own: every start runs it as it is in the DSH home
  now. A profile changed to another transport makes the next start fail
  with `start-failed`; `dshd rm web-1` and a new `run` recreate the
  instance for it.

## JSON shapes

- `get`: the status object. `state` is one of running, starting, stopping,
  failed, stopped, interrupted, previous-boot, masked, orphaned,
  inconsistent. `lastRun.ending` is one of stopped, stopped-without-intent,
  exited, failed, interrupted, or null while the run is in progress.
  `resources` contains live systemd counters, or null values when unavailable.
  `remedy` is the command to run next, or null.
- `ls`: `{"instances":[{"name","state","transport","release","releaseState","enabled","since","remedy","labels"}],"linger","warnings"}`.
- Mutating verbs: the status object plus `outcome` and `warnings`.
  `outcome` is one of created, started, already-running, stopped,
  already-stopped, restarted, updated, unchanged, enabled, already-enabled,
  disabled, already-disabled, removed, labelled, ended, still-running.
- `attach --json` for web: `{"origin","port","urlFile"}`, plus `url` with
  `--print-url`.
- Errors on stderr: `{"error":{"kind","message","hint"}}`. `hint` is the
  command to run next, when there is one.

## Error kinds and exit codes

| Exit | Kinds |
|------|-------|
| 1 | start-failed, stop-failed, release-invalid, io, inconsistent-state, systemctl-failed |
| 2 | invalid-argument, invalid-name, needs-target |
| 3 | not-found, release-not-found |
| 4 | exists-different-recipe, instance-running, task-already-ran, unit-masked, attached-elsewhere |
| 5 | not-running |
| 6 | unsupported-transport, unsupported-profile, autostart-not-allowed |
| 7 | systemd-unavailable, tool-missing, template-missing |
| 124 | still running at the check-in of `run` or `wait`; the instance continues |

`systemd-unavailable` means there is no user manager to talk to: log in as
the user (SSH, or `machinectl shell`), not through `su` or `sudo -u`.
`needs-target` lists the instances that exist; pick one and pass its name.
