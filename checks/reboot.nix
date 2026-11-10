# VM: crashes and reboots (P2.5, P3.5). A killed headless dsh, an instance
# running when the machine loses power and one running at a clean reboot
# each report what happened to them; an enabled instance comes back after
# both, with the DSH home as it was.
# catches: a headless prompt run again without --rerun, a crash or reboot
# reported as a clean stop, an enabled instance not coming back, a boot
# writing in the DSH home.
{ self, pkgs }:
let
  vm = import ./vm-node.nix { inherit self pkgs; };
in
pkgs.testers.runNixOSTest {
  name = "dshd-reboot";
  globalTimeout = 1800;
  nodes.machine = vm.node;
  testScript = vm.prelude + ''
    def back(before: str) -> None:
        """Waits for the boot and for the enabled instance's new run."""
        boot()
        machine.wait_until_succeeds(alice("systemctl --user is-active dshd@daily.service"))
        assert unit("daily", "InvocationID") != before
        pid = child("daily")
        assert machine.succeed(f"cat /proc/{pid}/cgroup").strip().endswith(unit("daily", "ControlGroup"))


    def tasks() -> str:
        """How many times the stub dsh of `chore` has started its task."""
        return run("grep -c '^mode [0-9]* hang$' ~/chore.log || true").strip()


    def ran(times: int) -> None:
        """Waits until the stub dsh of `chore` has started its task `times` times."""
        machine.wait_until_succeeds(
            alice(f"test \"$(grep -c '^mode [0-9]* hang$' ~/chore.log)\" = {times}"), timeout=WAIT
        )


    machine.start(allow_reboot=True)
    setup()

    # catches: a killed headless dsh restarted, its end misreported, the
    # instance allowed to autostart, or its prompt resubmitted by an
    # idempotent start rather than only by `start --rerun`
    with subtest("P2.5 killed headless"):
        run_bg("--name chore -- headless 'tidy the notes'", "hang", env="DSH_STUB_LOG=$HOME/chore.log")
        ran(1)
        runner = unit("chore", "MainPID")
        machine.succeed(f"kill -KILL {child('chore')}")
        wait("chore")
        report = get("chore")
        assert report["state"] == "failed" and report["lastRun"]["child"]["signal"] == 9, report
        assert unit("chore", "NRestarts") == "0" and unit("chore", "ActiveState") != "active"
        machine.fail(f"test -e /proc/{runner}")
        code, err = attempt("dshd enable --json chore")
        assert code == 6 and "autostart-not-allowed" in err, err
        code, err = attempt("dshd start --json chore")
        assert code == 4 and "task-already-ran" in err, err
        assert tasks() == "1"
        assert dshd("start --rerun chore")["state"] == "running"
        ran(2)
        machine.succeed(f"kill -KILL {child('chore')}")
        wait("chore")
        assert get("chore")["state"] == "failed"
        receipt("chore", "P2.5")

    # catches: MainPID loss being treated as a restartable child crash,
    # child status invented after the runner died, or a live cgroup left behind
    with subtest("P2.5 killed runner"):
        run_bg("--name owner-loss -- web", "web")
        cgroup = unit("owner-loss", "ControlGroup")
        invocation = unit("owner-loss", "InvocationID")
        machine.succeed(f"kill -KILL {unit('owner-loss', 'MainPID')}")
        until_unit("owner-loss", ActiveState="failed")
        report = get("owner-loss")
        assert report["state"] == "failed" and report["lastRun"]["ending"] == "failed", report
        assert report["lastRun"]["invocationId"] == invocation and report["lastRun"]["child"] is None, report
        assert unit("owner-loss", "NRestarts") == "0"
        machine.fail(f"test -s /sys/fs/cgroup{cgroup}/cgroup.procs")
        receipt("owner-loss", "P2.5")

    run_bg("--name daily -- web", "web")
    assert dshd("enable daily")["outcome"] == "enabled"
    run_bg("--name crashed -- web", "web")
    home = snapshot("~alice/.dsh")

    # catches: an instance cut by power loss reported as stopped, or an enabled
    # instance not coming back after it
    with subtest("P2.5 crash"):
        daily = unit("daily", "InvocationID")
        machine.succeed("sync")
        machine.crash()
        machine.start(allow_reboot=True)
        back(daily)
        report = get("crashed")
        assert report["state"] == "interrupted" and report["lastRun"]["ending"] == "interrupted", report
        assert unit("crashed", "ActiveState") != "active"
        receipt("crashed", "P2.5")

    # catches: a clean reboot reported as a crash or as dshd's own stop, an
    # enabled instance not coming back, a boot writing in the DSH home, a
    # stale state blocking start, a failed task forgotten by a reboot
    with subtest("P3.5 enabled comes back"):
        assert dshd("start crashed")["outcome"] == "started"
        daily = unit("daily", "InvocationID")
        machine.reboot()
        back(daily)
        report = get("crashed")
        assert report["state"] == "previous-boot", report
        assert report["lastRun"]["ending"] == "stopped-without-intent", report
        assert get("chore")["state"] == "failed"
        assert snapshot("~alice/.dsh") == home
        assert "dshd" not in run("systemctl --user list-timers --all --no-legend")
        assert dshd("start crashed")["outcome"] == "started"
        assert get("crashed")["state"] == "running"
        receipt("daily", "P3.5")

    finish(["P2.5", "P3.5"])
  '';
}
