# dshd.nix

Durable native DSH sessions on NixOS.

dshd keeps a `dsh` process running under your own systemd user manager
after the terminal, the SSH connection or the browser tab that started it
is gone, and brings it back after a reboot. It launches `dsh` exactly as a
terminal would, with the same arguments, environment and working
directory, and owns nothing else: systemd supervises, journald logs, Nix
pins the release, DSH keeps its sessions, settings, credentials and
plugins.

The shape is borrowed from [microvm.nix](https://github.com/microvm-nix/microvm.nix):
a flake exports a NixOS module, a package and `lib.mkRelease`; the module
installs one static unit template and a registry of pinned releases; an
imperative command runs, inspects, attaches to, updates, stops and removes
instances at runtime, with no rebuild. dshd does not package DSH: the
system configuration supplies `dsh`, and dshd runs the release it is given.

## At a glance

- One instance is one unit, `dshd@NAME.service`, in the invoking user's
  systemd user manager, and one directory, `~/.local/state/dshd/NAME`
  (systemd's `%S/dshd/NAME`).
- The unit's main process is `dshd-run`, the per-release runner. It spawns
  `dsh` with the recorded argv, cwd and environment and holds its pipes for
  the life of the unit, so no client owns the process.
- Instances run as you, with full native permission by default and no
  sandbox. dshd adds no daemon: the state directory and `systemctl show`
  are the whole truth, read again on every command.
- Releases are Nix store paths. An instance keeps running the exact bytes
  it started with, across switches, rollbacks and garbage collection, until
  you deliberately update and stop or restart it.
- dshd only launches DSH. An instance runs its DSH profile live from its
  DSH home, with the argv as typed, and dshd writes nothing in that home.
- Web instances keep their origin and default port across restarts;
  `attach --print-url` gives the current login token. ACP instances keep
  their sessions across controllers: a client can detach mid-turn and
  another can attach and receive what it missed.
- A headless task runs once. dshd never submits its prompt again unless you
  ask for it with `dshd start --rerun NAME`.
- Output is JSON when stdout is not a terminal, so scripts and agents read
  the same objects people see as text. `dshd --skill` prints a guide for
  agents.

```
%S/dshd/NAME/
  instance.json   the recipe (0600): id, release name, cwd, args, environment, profile, transport, labels
  last-run.json   how the current or last run started and, once it ended, how it ended
  current         -> /nix/store/...-dshd-release-...   the release the next start uses (GC root)
  booted          -> /nix/store/...-dshd-release-...   the running release; current again after a stop (GC root)
  web-port        the pinned port of a web instance
  lock            taken by every command that changes the instance
%t/dshd/NAME/     runtime directory (0700): web-url (0600), acp.sock (0600), stop-intent
```

## Profiles and capabilities

dshd reads the bundles a profile's `package.json` lists and gives the
instance one transport. A bundle is matched by the part of its name after
the last `/`, so `@deepseek-ai/dsh-web-app` and `dsh-web-app` are the same;
when a profile lists several, `dsh-headless` wins, then `dsh-web-app`, then
`dsh-acp-app`. The transport decides how the instance becomes ready, how
you attach to it and whether a crash restarts it.

| Profile (DSH bundle) | Transport | Ready when | stdin | stdout | Attach | Restart after a crash | Restrictions |
|---|---|---|---|---|---|---|---|
| `web` (`dsh-web-app`) | web | the first `dsh web: URL` line | `/dev/null` | scanned for the URL, then the journal | `dshd attach` prints the origin; `--print-url` the full URL | yes | `--no-open` and `--port` are injected unless given; `--public-url` passes through and suppresses the port |
| `acp` (`dsh-acp-app`) | acp | spawned and the relay socket bound | the relay | the relay | `dshd attach` runs the relay client on stdio | yes | one controller at a time; `--takeover` replaces it; the journal holds stderr only |
| `headless` (`dsh-headless`) | none | spawned | `/dev/null` | the journal | refused (`unsupported-transport`); `dshd read --follow NAME` | never | the task runs once; no `restart`, no `enable`; `start --rerun` runs it again on purpose |
| `dsh-sdk-app`, `dsh-sdk-minimal`, or no app bundle | refused at creation | | | | | | `unsupported-profile`: an SDK host serves the client that starts it, and a terminal app needs the terminal that starts it |

A ready web or ACP instance whose `dsh` crashes is restarted by systemd
after five seconds; that includes DSH's own restart button. A headless
instance is never restarted, so a prompt is never sent twice.

ACP instances own their approvals: while no controller is attached, a
`session/request_permission` from DSH is held, never answered, and the next
controller that resumes that session receives it. If more output arrived
while nobody was attached than the relay keeps, the answer to the resume
carries `_meta.dshd.droppedFrames`, the number of frames lost, so the
client reloads the session from DSH instead of trusting the replay.
Stopping the instance answers held requests with `cancelled` before
closing DSH's stdin.

## Installation

Add the flake and enable the module on the NixOS host:

```nix
{
  inputs.dshd.url = "github:mecattaf/dshd.nix";
  inputs.dshd.inputs.nixpkgs.follows = "nixpkgs";

  outputs =
    { nixpkgs, dshd, ... }:
    {
      nixosConfigurations.host = nixpkgs.lib.nixosSystem {
        modules = [
          ./configuration.nix
          dshd.nixosModules.default
          (
            { pkgs, ... }:
            {
              dshd = {
                enable = true;
                releases.stable.dsh = pkgs.callPackage ./dsh.nix { };
              };
              users.users.alice.linger = true;
            }
          )
        ];
      };
    };
}
```

The options are `enable`, `package`, `releases.<name>.{dsh, environment,
unsetEnvironment}`, `defaultRelease` and `startupTimeout`; their
descriptions are in [`nixos-modules/dshd/options.nix`](nixos-modules/dshd/options.nix).
Instances, profiles and autostart are runtime data and have no option.
Release names follow the instance-name grammar, are never `default` and are
never shaped like an instance id (a UUID).

Enabling the module installs, for every user:

- `dshd@.service`, a template whose text holds systemd specifiers and
  fixed values, of the options only `startupTimeout`, and no store path,
  user name, home or port;
- `dshd.target`, wanted by `default.target`, which starts the instances
  you enable;
- `/etc/dshd/releases/<name>` for each release, and
  `/etc/dshd/releases/default` for `defaultRelease`, or for the only
  release when there is exactly one;
- the `dshd` command.

A release is any directory with this layout; `lib.mkRelease` builds one:

| Release path | Used for |
|---|---|
| `bin/dsh` | the DSH entrypoint the runner spawns; its store name is the version `ls` shows |
| `bin/dshd-run` | the runner, started through `current` and stopped through `booted` |
| `share/dshd/release.json` | `{"environment": {...}, "unsetEnvironment": [...]}` |

```nix
dshd.lib.mkRelease {
  inherit pkgs;
  dsh = pkgs.callPackage ./dsh.nix { };
  environment.DSH_TELEMETRY_DISABLED = "1";
}
```

A store path built this way can be run without declaring it, with
`dshd run --release /nix/store/...-dshd-release-... -- web`; the template
still comes from the module.

When `dsh` is a wrapper script, it must not pin the permission mode.

Try the command without installing it:

```
nix run github:mecattaf/dshd.nix -- help
```

## Quick start

A web instance, started from an SSH session that can end right after:

```
$ dshd run -- web
$ dshd attach web-1
$ dshd attach --print-url web-1
$ dshd attach --open web-1
```

`run` creates `web-1`, starts it and returns once DSH announced its
address. `attach` prints the origin, `http://127.0.0.1:PORT`; only
`--print-url` prints the full URL with its login token, and `--open` hands
it to `xdg-open`.

An ACP instance with an explicit name, working directory and label:

```
$ dshd run --name review --cwd ~/src/project --label agency=~/agencies/x -- acp
$ dshd attach review
$ dshd attach --takeover review
$ dshd get --json review
$ dshd update -R review
$ dshd enable review
$ dshd ls
```

An ACP client uses `dshd attach NAME` as its agent command: it attaches,
copies stdio to the relay until either side ends, and detaches without
cancelling anything. `enable` makes the instance start with the user
manager, which with linger means at boot.

A headless task, followed in the foreground or picked up later:

```
$ dshd run --name notes -- headless "summarise the open issues"
$ dshd run --bg --name suite -- headless "run the test suite and fix what fails"
$ dshd wait --timeout 900 suite
$ dshd read suite
```

A foreground headless `run` prints DSH's output until `dsh` exits and
exits as it did. Ctrl+C ends the command, not the task; at `--timeout`
(180 seconds by default) `run` exits 124 and the task continues.
`dshd wait NAME` picks it up and `dshd read NAME` prints what it wrote.

## Verb reference

`dshd VERB [FLAGS] [NAME] [-- DSH ARGUMENTS]`. Flags come before `--`, and
only `run` takes arguments after it: they are passed to `dsh` byte for
byte, and `--` is required. `dshd` alone is `dshd ls`. There are no
aliases. `dshd help VERB`, or `dshd VERB --help`, is the authoritative
reference for each verb.

| Verb | Flags | Outcomes | What it does |
|---|---|---|---|
| `run -- ARGS` | `--bg`, `--name N`, `--release R`, `--cwd D`, `--label K=V`..., `--timeout S` | `created`, `started`, `already-running`, `ended`, `still-running` | Creates the instance, or matches the stored recipe of `--name`, and starts it. Web and ACP instances, and any instance with `--bg`, are left running. A headless instance runs in the foreground until `dsh` exits or `--timeout` passes. A different recipe under an existing name is refused. |
| `start NAME` | `--rerun` | `started`, `already-running` | Starts the instance again from its recorded recipe and its `current` release. Never creates one. A headless instance whose task ran is refused unless `--rerun` is given. |
| `stop NAME` | `--invocation ID` | `stopped`, `already-stopped` | Stops the unit and every process it started. The recipe, the release roots and DSH's sessions stay. With `--invocation ID`, only the run of that invocation: any other is refused with `not-running`. |
| `restart NAME` | | `restarted` | Clears a failed state, restarts on the `current` release and waits until the instance runs. Refused for headless instances. |
| `update NAME` | `-R` | `updated`, `unchanged` | Points `current` at what the recorded release name resolves to now and prints `nix store diff-closures` between the two releases; `-R` also restarts a running web or ACP instance. Refused for an instance created from a store path. |
| `ls` | | | Every instance: `NAME: state(release)`, with the command to run when one is needed. |
| `get NAME` | | | Everything dshd knows about one instance: the unit, the recipe, the release links, the profile and its DSH home, the web address or the ACP relay's report, how the last run ended, and whether linger is on. |
| `read NAME` | `-n LINES`, `-f` | | The journal of the current run, or of the last one, as DSH wrote it (200 lines by default). `-f` follows until the run ends. |
| `wait NAME` | `--timeout S` | `ended`, `still-running` | Waits until the current run ends, then exits as `dsh` did. A run that ended already is reported at once. |
| `attach NAME` | `--via web\|acp`, `--takeover`, `--print-url`, `--open` | | Never starts anything. Web: prints the origin, or the full URL with `--print-url`, or opens it with `--open`. ACP: runs the relay client of the instance's own release on stdio. |
| `label NAME K=V...` | | `labelled` | Sets labels; an empty value removes the key. `dsh` never sees labels, and the instance keeps running. |
| `enable NAME` | | `enabled`, `already-enabled` | Adds the unit to `dshd.target`, which the user manager starts at its first login, or at boot with linger. Starts nothing. Refused for headless instances. |
| `disable NAME` | | `disabled`, `already-disabled` | Removes the unit from `dshd.target`. Stops nothing. |
| `rm NAME` | | `removed` | Refused while running. Removes the instance directory and its release roots. The profile and DSH's sessions are untouched. |
| `help [VERB]` | | | The general page, or the page of one verb. |
| `--skill` | | | Prints the guide for agents: which verb for which need, the JSON shapes and the traps. |
| `--version` | | | Prints `dshd 0.1.0`. |

Every verb also takes `--json` and `--text`, except `help`, `--skill` and
`--version`, which always print text; `read` and `attach` take only
`--json`.

Names match `^[a-z0-9]([a-z0-9_-]{0,61}[a-z0-9])?$` and are never shaped
like an id (a UUID), so a name and an id never share a spelling. Without
`--name`, `run` always creates a new instance named `<profile>-<k>` with
the lowest free `k`. Every creation gets a new `id`, so a removed and
recreated name is a different instance to anything that keeps references.

## Profiles and accounts

Profiles are DSH's own, and dshd neither copies nor changes them.
`dshd run -- web` runs the same `dsh web` you would type, with every token
as typed, and DSH composes the profile at every start from what is in
`$DSH_HOME/profiles/<p>` then: the bundles in `package.json`, then
`cordis.patch.yml`, then any `--patch`. dshd writes nothing in the DSH
home. What an instance keeps across starts is its release, the `dsh`
build and the runner, until `dshd update`.

dshd reads one thing from the profile: the bundles in its `package.json`,
which give the instance its transport at creation. The transport is
recorded, and a start fails before `dsh` runs when the profile now selects
another one: `dshd start` reports `start-failed`, and `dshd read NAME`
says which. Restore the profile, or recreate the instance for it with
`dshd rm NAME` and `dshd run --name NAME -- ARGS`; sessions continue,
because DSH keeps them per home and working directory.

A profile DSH ships, such as `web`, `acp` or `headless`, that does not exist
yet in the home is created by DSH itself: dshd runs
`dsh --profile <p> --dump-default-config` in the instance's directory and
environment, then reads the `package.json` DSH wrote. For any other
missing profile, DSH's refusal is the error.

- Editing a profile changes every instance of it as it would change `dsh`
  typed by hand: at its next start, or at once in a running instance whose
  profile enables DSH's HMR.
- An instance's settings UI and plugin manager write into the profile, as
  they do for `dsh` typed by hand.
- `dsh --profile <p> --dump-config` shows what an instance composes at its
  next start.
- `--from-default-profile` is refused, because it would recreate the
  profile from its template at every start; run it once with `dsh`
  directly, then give the profile alone.
- `plugin`, `--dump-*`, `-V` and `--version` are refused: they manage
  plugins or print and exit, so there is no session to hold.

An account is a DSH home. `DSH_HOME=~/.dsh-work dshd run -- web` runs an
instance in that home; dshd records the resolved home, sets it explicitly
for `dsh`, and shows it in `get`. Credentials and sessions stay in the
home and are shared live by every instance of it. There is no account
registry, profile store or `--home` flag.

## Releases, update and retention

`--release R` takes a registry name, or an absolute store path; without it,
`run` uses `default`. The instance records the name. Its `current` link
is rooted for the garbage collector at creation, and the runner points
`booted` at `current` on every start. Both are registered GC roots, so a
running instance keeps its bytes across any switch, rollback or
`nix-collect-garbage -d`. After a stop, `booted` is repointed at `current`;
`lastRun` still identifies the release that ended. This releases an older
closure once no other root retains it.

| Release state | Meaning | Remedy |
|---|---|---|
| `current` | the links agree with the registry | |
| `stale` | `current` is the registry's release, `booted` is older | `dshd restart NAME` |
| `outdated` | the registry points at a different release | `dshd update NAME` |
| `missing` | the recorded name is gone from the registry | recreate with another `--release` |

`dshd update NAME` re-resolves the recorded name, repoints `current` and
lists what changed; the running process is untouched until
`dshd restart NAME`, or use `dshd update -R NAME` for both. A headless
instance is not restarted by `-R`: the update applies at its next
`dshd start --rerun NAME`. An instance created from a store path has
nothing to follow and is refused. The registry name `default` is reserved
for the alias selected by `defaultRelease`, or by the only named release;
name the configured release `stable`, for example. `dshd rm` deletes the links, so the next
collection frees a release no other instance holds.

## Environment and permission

At creation dshd captures the environment of the `dshd run` command,
minus a closed list, and stores it in `instance.json` (0600). There is no
`--env` flag: `K=V dshd run ...` and `env -u K dshd run ...` are the
interface. `get` shows names only, never values, and every later start
reuses the captured environment. That includes any secret exported in the
shell: name it in the release's `unsetEnvironment`, or start with
`env -u NAME dshd run ...`, to keep it out of the record.

| Never captured | Names |
|---|---|
| systemd service variables | `NOTIFY_SOCKET`, `LISTEN_PID`, `LISTEN_FDS`, `LISTEN_FDNAMES`, `WATCHDOG_PID`, `WATCHDOG_USEC`, `INVOCATION_ID`, `JOURNAL_STREAM`, `MANAGERPID`, `MANAGERPIDFDID`, `SYSTEMD_EXEC_PID`, `RUNTIME_DIRECTORY`, `MEMORY_PRESSURE_WATCH`, `MEMORY_PRESSURE_WRITE`, `STATE_DIRECTORY`, `CACHE_DIRECTORY`, `LOGS_DIRECTORY`, `CONFIGURATION_DIRECTORY`, `CREDENTIALS_DIRECTORY`, `FDSTORE`, `MAINPID`, `PIDFILE`, `LOG_NAMESPACE`, `SERVICE_RESULT`, `EXIT_CODE`, `EXIT_STATUS`, `MONITOR_SERVICE_RESULT`, `MONITOR_EXIT_CODE`, `MONITOR_EXIT_STATUS`, `MONITOR_INVOCATION_ID`, `MONITOR_UNIT`, `TRIGGER_UNIT`, `TRIGGER_PATH`, `TRIGGER_TIMER_REALTIME_USEC`, `TRIGGER_TIMER_MONOTONIC_USEC`, `REMOTE_ADDR`, `REMOTE_PORT` |
| SSH connection | `SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY` |
| login session | `XDG_SESSION_ID`, `XDG_SESSION_CLASS`, `XDG_SEAT`, `XDG_VTNR` |
| shell bookkeeping | `PWD`, `OLDPWD`, `SHLVL`, `_` |
| another supervisor's per-process identity, which would let it end or adopt the instance's processes | `HC_SCOPE`, `HC_RUNTIME`, `HC_TERMINAL_ID` |
| DSH's own identity, which `dsh` sets in the shells of the profile it launched | `DSH_PROFILE` |
| taken from the user manager at each start, or left unset when it has none | `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `SSH_AUTH_SOCK`, `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`, `DESKTOP_SESSION` |
| the release's `unsetEnvironment` | stripped at creation and again at every start |

At every start the runner composes the environment of `dsh` from scratch,
in this order, a later step never overriding a name an earlier one set
unless it says so:

1. the recorded environment;
2. the session variables of the user manager;
3. the strip of the release's `unsetEnvironment` and of the names above;
4. the release's `environment`, filling only names still unset;
5. `DSH_HOME` and `PWD`, always set;
6. `INVOCATION_ID` and `JOURNAL_STREAM` from the runner.

`DSH_PERMISSION_MODE=danger-full-access` is the default through every
release, so a retained instance never waits for an approval nobody sees.
Your own value, a profile patch and a home patch still win; set another
mode in the release's `environment`, for example `"workspace-write"`, to
change the default. dshd changes nothing about DSH telemetry; set
`DSH_TELEMETRY_DISABLED = "1"` in a release's `environment` to turn it off.

## Operating guide

**Linger.** Without linger, the user manager, and every instance with it,
stops when your last session ends. Set `users.users.<name>.linger = true`.
`run` and `enable` warn when it is off, and `get` and `ls --json` report
it as `linger`.

**Stopping.** `dshd stop` writes a stop intent into the runtime directory,
then stops the unit. The runner hands the stop to `dsh` (an ACP instance
first answers held requests with `cancelled` and closes DSH's stdin);
whatever is left after 30 seconds is killed. A stop ends every process of
the instance, tool children started with `nohup` or `&` included: nothing
a session started survives it. The last run's `ending` says who stopped
it: `stopped` when dshd did, `stopped-without-intent` for `systemctl
stop`, a logout without linger or a shutdown.

**Stopping one run.** A name outlives its runs: `start`, `start --rerun`,
a restart after a crash or a switch can put a newer invocation under it.
A supervisor that keeps the id of the run it started stops that run, and
never a newer one, with `dshd stop --invocation ID NAME`. ID is the
`invocationId` that `run --json`, `get` and `wait` print, or
`lastRun.invocationId` once the manager has unloaded an ended unit. The
unit is stopped only while it runs ID, and a stopped instance whose last
run was ID is `already-stopped`. Another invocation, running or last, and
an instance that never ran are refused with `not-running` (exit 5), and
nothing is stopped; `dshd get NAME` shows what runs instead. The
comparison and the stop hold the instance lock, so `run`, `start`,
`restart` and `update -R` of the name wait for both. systemd has no
conditional stop, so the fence is compare, then stop: a restart systemd
makes on its own after `RestartSec`, or a start from outside dshd
(`systemctl --user start`, `dshd.target`), takes no lock. After `stopped`,
`lastRun.invocationId` names the run that ended.

**Restart policy.** The template says `Restart=no` and
`RestartForceExitStatus=75`. The runner exits 75 only when a web or ACP
`dsh` that became ready dies without a stop request, so that is the only
restart: after `RestartSec=5s`, with at most five starts in 1800 seconds,
after which the instance is `failed` with `start-limit-hit`. The initial
start and DSH's own restart request (exit 42) both count toward that limit.
That is why
`systemctl --user status dshd@NAME` can show `status=75/TEMPFAIL` for a
crash handed back to systemd; `dshd get` shows the child's own end in
`lastRun.child`. A start that fails before readiness is reported with the
journal tail, never retried. A missing working directory or an unreadable
record makes the runner exit 78, also never retried. `dshd restart` and
`dshd start` clear a failed state, a start limit included, before they
start again. Tune the policy with your own drop-in, or override a value
in the configuration with `lib.mkForce`, for example:

```nix
systemd.user.services."dshd@".serviceConfig.TimeoutStopSec = lib.mkForce "120s";
```

A key the template does not set, such as `MemoryMax`, needs no `mkForce`.

**Memory pressure.** The template sets `OOMPolicy=continue`: when the
kernel kills one process of an instance for memory, the unit keeps
running. If that process was `dsh` itself, a web or ACP instance restarts
as after any crash, and a headless one ends `failed`.

**Headless tasks.** A headless `dsh` runs its prompt once. Once it ran,
`dshd start NAME` refuses with `task-already-ran`, and `restart`, `enable`
and `update -R` never start it; `dshd start --rerun NAME` submits the
recorded prompt again, on purpose. A crash, a kill or a reboot never
resubmits it.

**From inside an instance.** An agent inside DSH may call dshd itself.
`run` from inside creates a sibling instance with its own unit; the
caller's systemd identity is never captured. `stop`, `restart` or
`update -R` of the caller's own instance end the calling process with it,
and systemd still completes the action; check the result from outside with
`dshd get NAME`.

**Readiness.** `dshd run` and `dshd start` return once the instance is
ready: the URL is announced (web), the relay socket is bound (ACP), or
`dsh` is spawned (headless), within `startupTimeout` (150 seconds by
default).

**Web.** dshd injects `--no-open`, and `--port 0` on the first start; the
announced default port is pinned in `web-port` and passed on every later
start, so a browser's cookie for that origin stays valid. The login token
can change with each run; use `attach --print-url` to obtain the current
URL. Your own `--port`, `--no-open` or `--public-url` always wins; an
explicit `--port 0` asks DSH for a new port each time. A port taken by another process makes the
start fail; dshd never signals the holder. The login token is replaced by
`REDACTED` in the journal and lives only in `%t/dshd/NAME/web-url`. An SSH
tunnel changes the origin, so the browser logs in again through it.

**Reading output.** `dshd read NAME` prints the journal of the current or
last run; dshd writes no log files. Each run starts with the journal line
`dshd-run: run started` and ends with `dshd-run: run ended: RESULT`. When
the lines do not reach back to the run's start, because of `-n` or because
journald rotated them away, stderr says so and `read --json` reports
`"complete": false`. An ACP instance's journal holds DSH's stderr only,
since its stdout is the ACP channel, and `read` says so on stderr.
`systemctl --user status dshd@NAME.service` shows the unit, and the last
line of `/proc/<pid>/cgroup` of any process names the instance that owns
it.

## Observing instances

`dshd get --json NAME` exposes `resources`: `cpuUsageNSec`,
`memoryCurrent`, `memoryPeak`, `tasksCurrent`, `ioReadBytes` and
`ioWriteBytes`. These are live systemd counters, or `null` when unavailable;
a stopped unit may retain its CPU total and peak memory. Systemd accounts
CPU time for every unit, and the template enables memory, tasks and IO
accounting. IO byte counts also need the `io` controller in the user
manager's cgroup. NixOS provides it through its default
`systemd.settings.Manager.DefaultIOAccounting = true`; a host that turns
that off can delegate the controller instead:

```nix
systemd.services."user@".serviceConfig.Delegate = "pids memory cpu io";
```

Without either, IO counts may be `null` while CPU, memory and tasks stay
available. dshd changes neither setting.

Journald already holds an instance's lane history. Read it directly:

```sh
journalctl --user -u dshd@NAME.service -o json --output-fields=MESSAGE_ID,MESSAGE,USER_INVOCATION_ID,UNIT_RESULT,EXIT_CODE,EXIT_STATUS,N_RESTARTS,CPU_USAGE_NSEC,MEMORY_PEAK,IO_METRIC_READ_BYTES,IO_METRIC_WRITE_BYTES
```

A span starts and ends under the same `USER_INVOCATION_ID`; `UNIT_RESULT`
names the failure cause. Systemd's lifecycle message IDs identify the
records:

| Event | `MESSAGE_ID` |
|---|---|
| started | `39f53479d3a045ac8e11786248231fbf` |
| stopped | `9d1aaa27d60140bd96365438aad20286` |
| failed | `be02cf6855d2428ba40df7e9d022f03d` |
| restart scheduled (`N_RESTARTS`) | `5eb03494b6584870a536b337290809b3` |
| resources consumed (`CPU_USAGE_NSEC`, `MEMORY_PEAK`, IO counts when available) | `ae8f7b866b0347b9af31fe1c80b127c0` |

Systemd writes the resources record at info level only for a run that
passed one of its thresholds: more than 1 s of CPU time, 64 MiB of peak
memory or 1 MiB of IO, or any IP traffic. A lighter run's record is at
debug level, which a user manager at the default `LogLevel=info` does not
write.

Use `-u 'dshd@*.service'` for all instance lanes. `dshd read NAME` gives one
run's output. dshd keeps no timeline store or resource projection.

## Coexistence and migration

dshd owns only the `dshd@*` units, `dshd.target`, `%S/dshd`, `%t/dshd`,
`/etc/dshd` and one `dshd.target.wants` link per enabled instance, and it
writes nothing in a DSH home. It never stops, restarts,
signals or lists any other user unit or process, never writes a unit file
or a drop-in, and never reloads the user manager to create or remove an
instance. `enable` links `dshd@NAME.service` to
`/etc/systemd/user/dshd@.service` in `dshd.target.wants` of the user
manager's persistent unit directory, `~/.config/systemd/user` unless the
manager started with another `XDG_CONFIG_HOME`, and runs `systemctl --user
daemon-reload`; `disable` runs `systemctl --user disable`, which reloads it
too. `systemctl --user add-wants` is not used: on NixOS it would also link
the template and the target into that directory as their store paths,
which would keep the instance on the template of the day it was enabled
and hide `dshd.target` from the next switch. A `dsh` you run by hand is
invisible to dshd. A unit you masked with `systemctl --user mask` stays
masked: `get` shows `masked`, and `start` refuses with `unit-masked` and
names the command that unmasks it.

The template renders `X-RestartIfChanged=false` (`restartIfChanged =
false`) and `X-StopOnRemoval=false`, and its text does not change between
releases, so no `nixos-rebuild switch`, rollback, dshd upgrade or removal of
the module restarts or stops a running instance. A switch does start
`dshd.target`, so an enabled instance that was stopped starts again. A
switch also clears failed states, a start limit included, and re-arms
enabled instances that failed. `dshd disable NAME` keeps one stopped.

To move a session from a terminal into dshd, end the terminal `dsh` and
start the same command from the same directory:
`dshd run --name NAME -- ARGS`. DSH resumes its sessions per home and
working directory as it would in the terminal.

## Output, JSON and exit codes

Results print as JSON when stdout is not a terminal, or with `--json`, and
as text on a terminal, or with `--text`. `read` and `attach` print text
unless `--json` is given; `help`, `--skill` and `--version` always print
text. Errors follow the same rule on stderr: `dshd: MESSAGE` and a remedy
line, or `{"error":{"kind":KIND,"message":TEXT,"hint":COMMAND|null}}`.

- `get` prints the status object: the unit (`state`, `activeState`,
  `subState`, `result`, `mainPid`, `restarts`, `invocationId`, `since`),
  the recipe (`transport`, `cwd`, `args`, `environmentNames`, `labels`),
  `resources`, `release`, `profile`, `web` or `acp`, `lastRun`, `enabled`, `linger` and
  `remedy`.
- A mutating verb prints the status object plus `outcome` and `warnings`;
  `run` and `wait`, once they followed a run, add `exitCode`, `signal` and
  `reason`.
- `ls` prints one object:
  `{"instances":[{"name","state","transport","release","releaseState","enabled","since","remedy"}],"linger":BOOL,"warnings":[...]}`.
- `read --json` prints `{"name","invocationId","complete","streams","lines"}`;
  `attach --json` on a web instance prints `{"origin","port","urlFile"}`,
  and `url` with `--print-url`.

[`tests/contract/dshd.schema.json`](tests/contract/dshd.schema.json)
describes every object.

| Vocabulary | Values |
|---|---|
| `state` | `running`, `starting`, `stopping`, `failed`, `stopped`, `interrupted` (the host or the user manager went down mid-run), `previous-boot`, `masked`, `orphaned` (a unit without a record), `inconsistent` (a record that contradicts itself) |
| `lastRun.ending` | `stopped` (by dshd), `stopped-without-intent` (by anything else), `exited` (`dsh` ended by itself, successfully), `failed`, `interrupted`, or `null` while the run is in progress |
| `outcome` | `created`, `started`, `already-running`, `stopped`, `already-stopped`, `restarted`, `updated`, `unchanged`, `enabled`, `already-enabled`, `disabled`, `already-disabled`, `removed`, `labelled`, `ended`, `still-running` |
| `release.state` | `current`, `stale`, `outdated`, `missing` |
| `transport` | `web`, `acp`, `none` |

| Exit | Meaning | Kinds |
|---|---|---|
| 0 | success | |
| 1 | operation failed | `start-failed`, `stop-failed`, `release-invalid`, `io`, `inconsistent-state`, `systemctl-failed` |
| 2 | usage | `invalid-argument`, `invalid-name`, `needs-target` |
| 3 | not found | `not-found`, `release-not-found` |
| 4 | conflict | `exists-different-recipe`, `instance-running`, `task-already-ran`, `unit-masked`, `attached-elsewhere` |
| 5 | not running | `not-running` |
| 6 | unsupported | `unsupported-transport`, `unsupported-profile`, `autostart-not-allowed` |
| 7 | environment unavailable | `systemd-unavailable`, `tool-missing`, `template-missing` |
| 124 | still running at the check-in of `run` or `wait`; the instance continues | |
| N | `run` and `wait` in text mode: `dsh`'s own exit code, or 128+S when signal S ended it | |

In JSON mode `run` and `wait` exit 0 once they followed a run, and the
outcome carries `dsh`'s end; branch on `outcome` and `exitCode`. A closed
pipe, as in `dshd ls | head`, ends the command quietly with 0.
`systemd-unavailable` means there is no user manager to talk to: log in as
the user (SSH, or `machinectl shell`), not through `su` or `sudo -u`.
`needs-target` lists the instances that exist.

The contract only grows: adding an optional field is the one compatible
change. Removing or renaming a field, changing its type or meaning, adding
a value to a closed vocabulary or renumbering an exit code is a breaking
change, made only with a **Breaking:** entry in the CHANGELOG. The unit
and target names, the paths under `%S/dshd`, `%t/dshd` and
`/etc/dshd/releases` and the journal markers are frozen the same way. Records on disk carry no version
number and are closed: a command or runner that acts on a record refuses
one with a field it does not know rather than guess.

## Limitations

- One ACP controller at a time; `--takeover` replaces it.
- SDK and terminal profiles are refused: they need the client or the
  terminal that starts them.
- No terminal: dshd allocates no TTY, and a profile that needs one gets
  none.
- ACP readiness means spawned and listening, not initialised; the first
  `initialize` may still be probing a provider.
- A headless task's prompt runs once; running it again is always an
  explicit `start --rerun`.
- `stop`, `restart` or `update -R` run from inside an instance end the
  caller with it; check the result from outside.
- `stop --invocation` fences only dshd's own starts; systemd's restart
  after a crash and starts from outside dshd are not held off.
- No daemon: every command reads the state directory and the user manager
  again, so nothing restarts a lost controller, and nothing needs to.
- No account registry and no declared instances: accounts are DSH homes,
  instances are runtime data.
- Linux with a systemd user manager and the NixOS module's template only.
- The checks run every runtime proof against a stub `dsh`. How a real
  `dsh` materialises a shipped profile with `--dump-default-config`, the
  URL line `dsh web` prints, `--port 0` and ACP session resume is modelled
  by the stub, not observed.

## Development

`nix flake check -L` on a KVM-capable x86_64-linux host runs every check:

| Check | What it proves |
|---|---|
| `cargo-test` | unit tests, every case under `tests/golden` run against a release built from the stub dsh with fake `systemctl`, `journalctl`, `nix-store` and `xdg-open`, and the contract tests of the JSON objects |
| `clippy`, `rustfmt`, `nixfmt`, `deadnix` | lints and formatting, warnings as errors |
| `module` | the exact rendered template, the release registry and the name grammar |
| `hygiene` | spelling, no private paths, no unfinished markers, the licence and the attribution of adapted files |
| `lifecycle`, `transport`, `switch`, `reboot` | NixOS VM tests of the whole lifecycle with the stub dsh |

`tests/stub/dsh.sh` is the one fake DSH: `DSH_STUB_MODE` selects web, ACP,
headless or a failure mode, `DSH_STUB_PORT` the web port, `DSH_STUB_DELAY`
a slow headless task, `DSH_STUB_BACKGROUND` a tool process that ignores
SIGTERM, `DSH_STUB_RUN_AFTER_READY` a command run from inside the instance,
and `DSH_STUB_LOG` where it records each invocation. Each VM check writes
one receipt line per proof to its output and fails unless every proof it
names passed. `nix fmt` formats the Nix files. `cargo fmt`, `cargo clippy`
and `cargo test` work with any Rust toolchain of version 1.89 or newer;
outside Nix, where `DSHD_GOLDEN_RELEASE` is unset, the golden cases are
checked for their shape only.

A consumer flake keeps one nixpkgs with
`inputs.dshd.inputs.nixpkgs.follows = "nixpkgs"`.

## License

Apache-2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).
