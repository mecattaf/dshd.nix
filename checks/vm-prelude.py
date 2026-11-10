# The python every dshd VM check starts with; vm-node.nix prepends
# HELD_PORT. alice is the lingering test user: every dshd and
# systemctl --user command runs in a login shell of hers, bound to her own
# user manager. bob does not linger and is reached over SSH only. Proofs
# read dshd's JSON output, the user manager's properties and the kernel's
# view of the processes, and the human form only where it is what they
# prove; each proof appends one receipt line.
import base64
import datetime as dt
import json
import shlex
from typing import Any

RECEIPTS = "/root/dshd-receipts.ndjson"
STATE = "~alice/.local/state/dshd"
WAIT = dt.timedelta(minutes=3)
# How long the user manager may take to reap a process it killed.
REAPED = dt.timedelta(seconds=30)
PROFILES: dict[str, list[str]] = {
    "web": ["@deepseek-ai/dsh-web-app"],
    "acp": ["@deepseek-ai/dsh-acp-app"],
    "headless": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"],
}
FIXTURES = "bystander-terminal.service bystander-web.service"


def alice(command: str) -> str:
    """A root shell command that runs `command` as alice."""
    session = (
        "set -eu; "
        'rt="$(loginctl show-user alice --property=RuntimePath --value)"; '
        'export XDG_RUNTIME_DIR="$rt" DBUS_SESSION_BUS_ADDRESS="unix:path=$rt/bus"; '
    )
    return "su -l alice -c " + shlex.quote(session + command)


def run(command: str) -> str:
    """`command` as alice; a non-zero exit fails the check."""
    return machine.succeed(alice(command), timeout=WAIT)


def attempt(command: str) -> tuple[int, str]:
    """`command` as alice: its exit status and its stderr."""
    return machine.execute(alice(f"{command} 2>&1 >/dev/null"), timeout=WAIT)


def dshd(command: str) -> Any:
    """`dshd VERB --json ARGS` as alice, parsed."""
    verb, _, rest = command.partition(" ")
    return json.loads(run(f"dshd {verb} --json {rest}"))


def run_bg(spec: str, mode: str, env: str = "") -> Any:
    """`dshd run --bg --json SPEC` as alice, the stub dsh in `mode`; the
    mode and every assignment in `env` are recorded at creation."""
    return json.loads(run(f"env DSH_STUB_MODE={mode} {env} dshd run --bg --json {spec}"))


def get(name: str) -> Any:
    return dshd(f"get {name}")


def listing() -> list[Any]:
    return json.loads(run("dshd ls --json"))["instances"]


def wait(name: str) -> Any:
    """`dshd wait --json NAME`: returns once the instance's run has ended."""
    return dshd(f"wait {name}")


def until_unit(name: str, **want: str) -> None:
    """Waits until the user manager reports each property of
    dshd@NAME.service with its value in `want`: for what systemd does on
    its own, such as a restart after a crash."""
    checks = " && ".join(
        f'test "$(systemctl --user show --property={prop} --value dshd@{name}.service)" = {shlex.quote(value)}'
        for prop, value in want.items()
    )
    machine.wait_until_succeeds(alice(checks), timeout=WAIT)


def unit(name: str, prop: str) -> str:
    """One property of dshd@NAME.service, as the user manager reports it."""
    return run(
        f"systemctl --user show --property={prop} --value dshd@{name}.service"
    ).strip()


def child(name: str) -> int:
    """The PID of the instance's dsh: the one child of the runner."""
    runner = unit(name, "MainPID")
    pids = machine.succeed(f"pgrep -P {runner}").split()
    assert len(pids) == 1, f"{name}: the runner {runner} has children {pids}"
    return int(pids[0])


def logged(log: str, key: str) -> int:
    """The PID of the first `KEY PID` line the stub wrote to ~alice/LOG."""
    return int(run(f"sed -n 's/^{key} //p' ~/{log}").split()[0])


def reaped(pid: int) -> None:
    """Waits until process PID is gone, reaped by whoever killed it."""
    machine.wait_until_fails(f"kill -0 {pid}", timeout=REAPED)


def proc(pid: int, entry: str) -> list[bytes]:
    """/proc/PID/ENTRY (cmdline or environ), split at NUL."""
    raw = base64.b64decode(machine.succeed(f"base64 -w0 /proc/{pid}/{entry}"))
    return raw.split(b"\0")[:-1]


def native_args(pid: int) -> list[str]:
    """The arguments the stub dsh received, after its own path."""
    cmdline = proc(pid, "cmdline")
    at = next(i for i, arg in enumerate(cmdline) if arg.endswith(b"/bin/dsh"))
    return [arg.decode() for arg in cmdline[at + 1 :]]


def environ(pid: int) -> dict[str, str]:
    pairs = [entry.decode().partition("=") for entry in proc(pid, "environ")]
    return {name: value for name, _, value in pairs}


def snapshot(path: str) -> str:
    """Every entry under `path` with inode, mode, size, mtime and ctime, so
    any write, replacement or metadata change shows."""
    return machine.succeed(
        f"find {path} \\( -type d -printf '%P %i %m\\n' \\)"
        " -o -printf '%P %i %m %s %T@ %C@ %l\\n' | sort"
    )


def fixtures() -> str:
    """MainPID and InvocationID of the two fixture user units."""
    return run(f"systemctl --user show --property=MainPID,InvocationID {FIXTURES}")


def unmanaged_dsh() -> int:
    """Starts a dsh by hand, outside dshd: the PID of that unmanaged sibling."""
    dsh = "/etc/dshd/releases/default/bin/dsh"
    run(f"DSH_STUB_MODE=web setsid -f {dsh} web </dev/null >/dev/null 2>&1")
    found = machine.wait_until_succeeds(f"pgrep -o -u alice -f '{dsh} web'")
    return int(found.strip())


def over_ssh(command: str, user: str = "alice") -> tuple[str, str]:
    """`command` as `user` in an SSH session that ends when it returns: the
    session's logind id and the command's stdout."""
    remote = "printf 'session %s\\n' \"$XDG_SESSION_ID\"; " + command
    out = machine.succeed(
        f"ssh -o StrictHostKeyChecking=no -o BatchMode=yes {user}@localhost "
        + shlex.quote(remote),
        timeout=WAIT,
    )
    first, _, rest = out.partition("\n")
    session = first.removeprefix("session ")
    assert first.startswith("session ") and session, out
    return session, rest


def receipt(name: str, proof: str, report: Any = None) -> None:
    """Appends the receipt of one passed proof about one instance, from
    `report`, or else from alice's `dshd get NAME`."""
    report = report or get(name)
    line = {
        "proof": proof,
        "instance": name,
        "id": report["id"],
        "invocationId": report["invocationId"],
        "release": (report["release"] or {}).get("booted"),
        "profile": (report["profile"] or {}).get("exposed"),
        "transport": report["transport"],
        "result": "pass",
    }
    machine.succeed(f"printf '%s\\n' {shlex.quote(json.dumps(line))} >> {RECEIPTS}")


def finish(proofs: list[str]) -> None:
    """Every named proof left a receipt; copy the receipts to $out."""
    want = shlex.quote(json.dumps(sorted(proofs)))
    machine.succeed(
        f"jq -es --argjson want {want} "
        "'(map(.proof) | unique) == $want' "
        + RECEIPTS
    )
    machine.copy_from_machine(RECEIPTS)


def boot() -> None:
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("default.target", "alice")


def setup() -> None:
    """Boots, writes alice's three profiles and lets root ssh in as alice
    and as bob."""
    boot()
    for profile, bundles in PROFILES.items():
        manifest = json.dumps({"dsh": {"profile": {"bundles": bundles}}})
        target = f"~/.dsh/profiles/{profile}"
        run(f"mkdir -p {target} && printf '%s\\n' {shlex.quote(manifest)} > {target}/package.json")
        run(f"printf '[]\\n' > {target}/cordis.patch.yml")
    machine.succeed(
        "install -d -m 0700 /root/.ssh",
        "ssh-keygen -q -t ed25519 -N '' -f /root/.ssh/id_ed25519",
    )
    for user in ("alice", "bob"):
        machine.succeed(
            f"install -d -o {user} -g users -m 0700 ~{user}/.ssh",
            f"install -o {user} -g users -m 0600 /root/.ssh/id_ed25519.pub ~{user}/.ssh/authorized_keys",
        )
    machine.wait_for_unit("sshd.service")
