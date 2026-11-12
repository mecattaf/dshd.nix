# VM: dshd's own `run --patch`, the overlay snapshotted into the instance
# at creation, with the stub dsh, which prints `stub-dsh: patch P sha256=D`
# to the journal for each --patch it receives; and `run --stdin`, a
# headless task's input snapshotted the same way, which the stub reads for
# the task `-` and reports as `stub-dsh: stdin sha256=D`.
# catches: an overlay read again from its source at a later start, a
# changed overlay accepted under an existing name, the same overlay refused,
# snapshots given to dsh out of order, after the overlays typed in the argv
# or not right after the profile, a snapshot lost by a restart systemd
# makes, `get` reporting another digest than the file holds, a snapshot left
# behind by `rm`, an instance without --patch whose record changed, a
# headless dsh that reads /dev/null although --stdin gave it a task larger
# than one argument, dshd's own stdin not taken for --stdin -, other input
# under an existing name accepted, and --stdin taken for a web profile.
{ self, pkgs }:
let
  vm = import ./vm-node.nix { inherit self pkgs; };
in
pkgs.testers.runNixOSTest {
  name = "dshd-run-patch";
  globalTimeout = 1800;
  nodes.machine = vm.node;
  testScript = vm.prelude + ''
    import hashlib

    SOURCE = "/tmp/p.yml"
    ONE = "model: one\n"
    X, Y = "x: 1\n", "y: 1\n"


    def digest(text: str) -> str:
        return hashlib.sha256(text.encode()).hexdigest()


    def write(path: str, text: str) -> None:
        """Writes `text` to `path` as alice."""
        run(f"printf %s {shlex.quote(text)} > {path}")


    def patch_lines(name: str, count: int) -> list[tuple[str, str]]:
        """The stub's patch lines in the journal of dshd@NAME, every run in
        order, as (path, digest) pairs, once there are `count` of them."""
        journal = (
            f"journalctl --user -u dshd@{name}.service --no-pager -o cat"
            " | sed -n 's/^stub-dsh: patch //p'"
        )
        machine.wait_until_succeeds(
            alice(f'test "$({journal} | wc -l)" -ge {count}'), timeout=WAIT
        )
        pairs: list[tuple[str, str]] = []
        for line in run(journal).splitlines():
            path, _, value = line.rpartition(" sha256=")
            pairs.append((path, value))
        assert len(pairs) == count, pairs
        return pairs


    def stdin_lines(name: str, count: int) -> list[str]:
        """The digests the stub read as its stdin, every run of dshd@NAME
        in order, once there are `count` of them."""
        journal = (
            f"journalctl --user -u dshd@{name}.service --no-pager -o cat"
            " | sed -n 's/^stub-dsh: stdin sha256=//p'"
        )
        machine.wait_until_succeeds(
            alice(f'test "$({journal} | wc -l)" -ge {count}'), timeout=WAIT
        )
        lines = run(journal).split()
        assert len(lines) == count, lines
        return lines


    def refused(spec: str) -> str:
        """`dshd run --bg --json SPEC` as alice, which must fail: its kind."""
        status, err = attempt(f"env DSH_STUB_MODE=headless dshd run --bg --json {spec}")
        kind = json.loads(err)["error"]["kind"]
        assert status == 4, (status, err)
        return kind


    start_all()
    setup()
    state = machine.succeed(f"echo {STATE}").strip()

    # catches: a start that reads the source again, so removing it loses the
    # overlay, or a rerun that gets another snapshot or digest
    with subtest("snapshot survives source removal"):
        write(SOURCE, ONE)
        assert run_bg(f"--name a --patch {SOURCE} -- headless t", "headless")["outcome"] == "created"
        wait("a")
        run(f"rm {SOURCE}")
        assert dshd("start --rerun a")["outcome"] == "started"
        wait("a")
        first, second = patch_lines("a", 2)
        assert first == second, (first, second)
        path, value = first
        assert path.startswith(f"{state}/a/patches/00-") and path.endswith(".yml"), path
        assert value == digest(ONE), value
        assert run(f"stat -c %a {state}/a/patches {path}").split() == ["700", "600"]
        receipt("a", "snapshot-survives-source")

    # catches: a changed overlay under an existing name accepted as the same
    # recipe
    with subtest("changed content is a different recipe"):
        write(SOURCE, "model: two\n")
        assert refused(f"--name a --patch {SOURCE} -- headless t") == "exists-different-recipe"
        receipt("a", "changed-content-refused")

    # catches: the same bytes refused as another recipe; a headless task
    # that ran is then the one refusal left
    with subtest("same content is the same recipe"):
        write(SOURCE, ONE)
        assert refused(f"--name a --patch {SOURCE} -- headless t") == "task-already-ran"
        receipt("a", "same-content-same-recipe")

    # catches: snapshots out of order, not right after --profile V, or after
    # the overlay typed in the argv, which must win
    with subtest("order and position"):
        write("~/x.yml", X)
        write("~/y.yml", Y)
        run_bg("--name ordered --patch ~/x.yml --patch ~/y.yml -- --profile web --patch z.yml", "web")
        x = f"{state}/ordered/patches/00-{digest(X)[:16]}.yml"
        y = f"{state}/ordered/patches/01-{digest(Y)[:16]}.yml"
        want = ["--profile", "web", "--patch", x, "--patch", y, "--patch", "z.yml"]
        args = native_args(child("ordered"))
        assert args[: len(want)] == want and args[len(want)] == "--no-open", args
        digests = [value for _, value in patch_lines("ordered", 3)]
        assert digests == [digest(X), digest(Y), "unreadable"], digests
        receipt("ordered", "order-and-position")

    # catches: a restart systemd makes on its own handing dsh another
    # overlay or none
    with subtest("web restart keeps the snapshot"):
        before = native_args(child("ordered"))[: len(want)]
        machine.succeed(f"kill -KILL {child('ordered')}")
        until_unit("ordered", NRestarts="1", ActiveState="active")
        assert native_args(child("ordered"))[: len(want)] == before
        lines = patch_lines("ordered", 6)
        assert lines[3:] == lines[:3] and lines[0] == (x, digest(X)), lines
        receipt("ordered", "web-restart-keeps-snapshot")
        dshd("stop ordered")

    # catches: get reporting a digest the snapshot does not have, or a
    # snapshot left behind by rm
    with subtest("get and rm"):
        report = get("a")
        (patch,) = report["patches"]
        held = run(f"sha256sum {state}/a/patches/{patch['file']}").split()[0]
        assert patch["sha256"] == held == digest(ONE) and patch["source"] == SOURCE, patch
        assert report["profile"]["patches"] == [], report["profile"]
        receipt("a", "get-and-rm", report)
        assert dshd("rm a")["outcome"] == "removed"
        machine.fail(f"test -e {state}/a")

    # catches: an instance made without --patch whose record or directory
    # changed
    with subtest("no patch, no change"):
        run_bg("--name plain -- headless t", "headless")
        wait("plain")
        record = json.loads(run(f"cat {state}/plain/instance.json"))
        assert "patches" not in record, record
        machine.fail(f"test -e {state}/plain/patches")
        assert get("plain")["patches"] == []
        receipt("plain", "no-patch-no-change")

    # catches: a headless dsh that reads /dev/null although --stdin gave it
    # a task larger than one argument may be, a rerun that reads the source
    # again, a snapshot with a loose mode, or one left behind by rm
    with subtest("stdin snapshot is the task"):
        brief = "/tmp/brief.md"
        run(f"head -c 140000 /dev/zero | tr '\\0' a > {brief}; echo >> {brief}")
        held = run(f"sha256sum {brief}").split()[0]
        assert run_bg(f"--name brief --stdin {brief} -- headless -", "headless")["outcome"] == "created"
        wait("brief")
        run(f"rm {brief}")
        assert dshd("start --rerun brief")["outcome"] == "started"
        wait("brief")
        assert stdin_lines("brief", 2) == [held, held]
        record = json.loads(run(f"cat {state}/brief/instance.json"))
        assert record["stdin"] == {"sha256": held, "source": brief}, record["stdin"]
        assert record["args"] == ["headless", "-"], record["args"]
        assert run(f"stat -c %a {state}/brief/stdin").strip() == "600"
        receipt("brief", "stdin-snapshot-is-the-task")
        assert dshd("rm brief")["outcome"] == "removed"
        machine.fail(f"test -e {state}/brief")

    # catches: --stdin - not reading dshd's own stdin, or other input under
    # an existing name accepted as the same recipe
    with subtest("stdin from dshd's stdin"):
        piped = "env DSH_STUB_MODE=headless dshd run --bg --json --name piped --stdin - -- headless -"
        assert json.loads(run(f"printf 'say ok\\n' | {piped}"))["outcome"] == "created"
        wait("piped")
        assert stdin_lines("piped", 1) == [digest("say ok\n")]
        status, err = attempt(f"printf 'say other\\n' | {piped}")
        assert (status, json.loads(err)["error"]["kind"]) == (4, "exists-different-recipe"), err
        receipt("piped", "stdin-from-dshd-stdin")

    # catches: --stdin taken for a profile whose dsh reads no task from
    # stdin, or an instance made before the refusal
    with subtest("web refuses stdin"):
        write("~/task.md", "say ok\n")
        status, err = attempt("dshd run --bg --json --name page --stdin ~/task.md -- web")
        assert (status, json.loads(err)["error"]["kind"]) == (2, "invalid-argument"), err
        machine.fail(f"test -e {state}/page")
        receipt("piped", "web-refuses-stdin")

    finish([
        "changed-content-refused", "get-and-rm", "no-patch-no-change", "order-and-position",
        "same-content-same-recipe", "snapshot-survives-source", "stdin-from-dshd-stdin",
        "stdin-snapshot-is-the-task", "web-refuses-stdin", "web-restart-keeps-snapshot",
    ])
  '';
}
