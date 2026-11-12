# VM: the lifecycle of instances with the stub dsh (P1.1-P1.3, P1.5-P1.7,
# P2.2, P2.4-P2.6, P3.4, P4.1-P4.3), the foreground run, an instance that
# restarts itself, a run's --patch overlays, and a user without linger.
# catches: an instance that depends on the session that started it, a
# changed argv or environment, a second process for one name, a restart
# policy other than the runner's exit 75, work left behind by a stop or a
# task end, a profile that now selects another transport started anyway,
# writes outside the instance or in the DSH home, an ending that misnames
# who stopped it, a secret outside its 0600 record, nix needed at start,
# Ctrl+C of a foreground run reaching the instance, output lost before the
# exit, a self-restart that loses the instance, a stop by invocation id
# that reaches a newer run of the name, and linger hidden or changed.
{ self, pkgs }:
let
  vm = import ./vm-node.nix { inherit self pkgs; };

  # Every nix tool, failing and leaving a mark in the runtime directory: put
  # first on the user manager's PATH, it shows any nix call during a start.
  nixCalled = pkgs.writeShellScript "nix-called" ''
    echo "$0 $*" >>"$XDG_RUNTIME_DIR/nix-called"
    exit 1
  '';
  nixMask = pkgs.runCommand "nix-mask" { } ''
    mkdir -p "$out/bin"
    for tool in nix nix-build nix-env nix-instantiate nix-store; do
      ln -s ${nixCalled} "$out/bin/$tool"
    done
  '';
in
pkgs.testers.runNixOSTest {
  name = "dshd-lifecycle";
  globalTimeout = 1800;
  nodes.machine = {
    imports = [ vm.node ];
    environment.etc."environment.d/50-dshd-paths.conf".text = ''
      XDG_STATE_HOME=$HOME/manager-state
      XDG_CONFIG_HOME=$HOME/manager-config
    '';
  };
  testScript = vm.prelude + ''
    NIX_MASK = "${nixMask}"
    CLOSED = ["SSH_CONNECTION", "SSH_CLIENT", "XDG_SESSION_ID", "SHLVL", "OLDPWD"]

    start_all()
    setup()

    # catches: state or enablement inferred from environment imported after
    # the manager resolved its specifiers and unit search paths
    with subtest("manager paths remain authoritative"):
        run("env XDG_STATE_HOME=$HOME/imported-state XDG_CONFIG_HOME=$HOME/imported-config"
            " systemctl --user import-environment XDG_STATE_HOME XDG_CONFIG_HOME")
        made = json.loads(run("env XDG_STATE_HOME=$HOME/client-state XDG_CONFIG_HOME=$HOME/client-config"
                              " DSH_STUB_MODE=web dshd run --bg --json --name paths -- web"))
        expected = machine.succeed(f"readlink -f {STATE}/paths").strip()
        assert unit("paths", "WorkingDirectory") == expected, made
        assert dshd("enable paths")["enabled"] is True
        assert "dshd@paths.service" in run("systemctl --user show --property=Wants --value dshd.target")
        assert dshd("disable paths")["enabled"] is False
        assert "dshd@paths.service" not in run("systemctl --user show --property=Wants --value dshd.target")
        dshd("stop paths")
        dshd("rm paths")
        for directory in ("manager-state", "imported-state", "client-state"):
            machine.fail(f"test -e ~alice/{directory}/dshd/paths")

    # catches: the instance living in the SSH session, or the unit's %S and
    # the CLI's state directory disagreeing; keyed on the SSH session's own
    # id, never on a listing that also shows the lingering manager session
    with subtest("P1.1 survives ssh end"):
        session, out = over_ssh("PLANTED=1 DSH_STUB_MODE=web dshd run --bg --json -- web")
        made = json.loads(out)
        assert made["outcome"] == "created" and made["name"] == "web-1", made
        machine.wait_until_fails(f"loginctl show-session {session}", timeout=WAIT)
        pid, runner = child("web-1"), unit("web-1", "MainPID")
        assert str(made["mainPid"]) == runner, (made, runner)
        assert made["invocationId"] == unit("web-1", "InvocationID"), made
        cgroup = unit("web-1", "ControlGroup")
        assert cgroup.endswith("/dshd@web-1.service"), cgroup
        assert machine.succeed(f"cat /proc/{pid}/cgroup").strip().endswith(cgroup)
        cwd = machine.succeed(f"readlink /proc/{runner}/cwd").strip()
        assert cwd == machine.succeed("readlink -f ~alice/.local/state/dshd/web-1").strip()
        assert get("web-1")["state"] == "running" and child("web-1") == pid
        receipt("web-1", "P1.1")

    # catches: a token altered on its way to dsh, a captured session or SSH
    # variable, or the permission default missing
    with subtest("P1.2 argv verbatim"):
        env = environ(child("web-1"))
        assert env["PLANTED"] == "1" and env["DSH_PERMISSION_MODE"] == "danger-full-access"
        assert [name for name in CLOSED if name in env] == [], env
        assert "PLANTED" in get("web-1")["environmentNames"]
        token = "a $HOME %s \" ' b\nc"
        run_bg(f"--name argv -- web {shlex.quote(token)}", "web")
        args = native_args(child("argv"))
        assert args == ["web", token, "--no-open", "--port", "0"], args
        assert get("argv")["args"] == ["web", token], "the record holds the argv as typed"
        receipt("argv", "P1.2")

    # catches: a second process for one name, already-running reported
    # before readiness, a second run of the same recipe spawning again (also
    # from another session's environment), two starts booting dsh twice, or
    # a different recipe replacing the first
    with subtest("P1.3 one unit one pid"):
        run(
            "for i in 1 2 3 4 5 6; do "
            "env DSH_STUB_MODE=web DSH_STUB_LOG=$HOME/conc.log dshd run --bg --json --name conc -- web >~/conc.$i 2>&1 & "
            "done; wait"
        )
        outcomes = [json.loads(run(f"cat ~/conc.{i}")) for i in range(1, 7)]
        assert sorted(str(o.get("outcome")) for o in outcomes) == ["already-running"] * 5 + ["created"], outcomes
        assert {o["invocationId"] for o in outcomes} == {unit("conc", "InvocationID")}, outcomes
        assert all(o["state"] == "running" for o in outcomes), outcomes

        def boots() -> str:
            """How many times the stub booted for conc: one mode line each."""
            return run("grep -c '^mode [0-9]* web$' ~/conc.log").strip()

        assert boots() == "1" and logged("conc.log", "mode") == child("conc")
        dshd("stop conc")
        stopped = unit("conc", "InvocationID")
        run("dshd start --json conc >~/conc.a 2>&1 & dshd start --json conc >~/conc.b 2>&1 || true; wait")
        both = [json.loads(run(f"cat ~/conc.{k}")) for k in "ab"]
        fresh = unit("conc", "InvocationID")
        assert fresh != stopped and all(o["state"] == "running" and o["invocationId"] == fresh for o in both), both
        assert boots() == "2", "two starts boot once"
        pid = child("conc")
        _, out = over_ssh("DSH_STUB_MODE=web dshd run --bg --json --name conc -- web")
        again = json.loads(out)
        assert again["outcome"] == "already-running" and (child("conc"), unit("conc", "InvocationID")) == (pid, fresh), again
        assert boots() == "2"
        pid, invocation = child("argv"), unit("argv", "InvocationID")
        again = run_bg(f"--name argv -- web {shlex.quote(token)}", "web")
        assert again["outcome"] == "already-running", again
        code, err = attempt("env DSH_STUB_MODE=web dshd run --bg --json --name argv -- web other")
        assert code == 4 and "exists-different-recipe" in err, err
        assert (child("argv"), unit("argv", "InvocationID")) == (pid, invocation)
        units = run("systemctl --user list-units --all --plain --no-legend 'dshd@argv*'")
        assert len(units.splitlines()) == 1, units
        receipt("conc", "P1.3")

    # catches: state held by a dshd process instead of the state directory
    # and the user manager
    with subtest("P1.5 no daemon"):
        before, invocation = listing(), unit("web-1", "InvocationID")
        run("systemctl --user daemon-reexec")
        machine.fail("pgrep -x dshd")
        assert listing() == before and unit("web-1", "InvocationID") == invocation
        receipt("web-1", "P1.5")

    # catches: a stop or crash reaching a sibling, an unmanaged dsh or a fixture
    # unit, a crashed web dsh not restarted, a stop or a task end leaving work
    # in or out of the cgroup, and the documented journald lane recipe missing
    # the restart or resources record of a crashed run
    with subtest("P1.6 isolation"):
        run_bg("--name iso-a -- web", "web", env="DSH_STUB_BACKGROUND=1 DSH_STUB_LOG=$HOME/iso-a.log")
        # Past systemd's 64 MiB threshold, so the manager writes the run's
        # resources record at info level.
        peak = "dd if=/dev/zero of=/dev/null bs=80M count=1"
        run_bg(
            "--name iso-b -- web",
            "web",
            env=f"DSH_STUB_LOG=$HOME/iso-b.log DSH_STUB_RUN_AFTER_READY={shlex.quote(peak)}",
        )
        run_bg("--name iso-c -- web", "web")
        unmanaged = unmanaged_dsh()
        others = fixtures()
        kept = (child("iso-c"), unit("iso-c", "InvocationID"))
        stopped = unit("iso-a", "ControlGroup")
        background = logged("iso-a.log", "background")
        assert machine.succeed(f"cat /proc/{background}/cgroup").strip().endswith(stopped)
        assert dshd("stop iso-a")["outcome"] == "stopped"
        reaped(background)
        machine.fail(f"test -s /sys/fs/cgroup{stopped}/cgroup.procs")
        machine.wait_until_succeeds(alice("grep -qx 'after-ready [0-9]* 0' ~/iso-b.log"), timeout=WAIT)
        victim, invocation = child("iso-b"), unit("iso-b", "InvocationID")
        machine.succeed(f"kill -KILL {victim}")
        until_unit("iso-b", NRestarts="1", ActiveState="active")
        report = get("iso-b")
        assert report["state"] == "running" and report["restarts"] == 1, report
        assert unit("iso-b", "InvocationID") != invocation and child("iso-b") != victim
        # Pin the documented read-only lane recipe to real manager records.
        fields = "MESSAGE_ID,MESSAGE,USER_INVOCATION_ID,UNIT_RESULT,EXIT_CODE,EXIT_STATUS,N_RESTARTS,CPU_USAGE_NSEC,MEMORY_PEAK,IO_METRIC_READ_BYTES,IO_METRIC_WRITE_BYTES"
        lane = f"journalctl --user -u dshd@iso-b.service --no-pager -o json --output-fields={fields}"
        resources_id = "ae8f7b866b0347b9af31fe1c80b127c0"
        restart_id = "5eb03494b6584870a536b337290809b3"
        machine.wait_until_succeeds(
            alice(lane + " | jq -e 'select(.MESSAGE_ID == \"" + resources_id + "\" and has(\"MEMORY_PEAK\"))'"),
            timeout=WAIT,
        )
        entries = [json.loads(line) for line in run(lane).splitlines()]
        assert any(e.get("MESSAGE_ID") == restart_id and int(e["N_RESTARTS"]) == 1 for e in entries), entries
        assert any(e.get("MESSAGE_ID") == resources_id and e.get("USER_INVOCATION_ID") == invocation
                   and int(e["MEMORY_PEAK"]) > 64 * 2**20 for e in entries), entries
        assert (child("iso-c"), unit("iso-c", "InvocationID")) == kept
        assert get("iso-a")["state"] == "stopped" and fixtures() == others
        machine.succeed(f"kill -0 {unmanaged}")
        run(
            "env DSH_STUB_MODE=headless DSH_STUB_BACKGROUND=1 DSH_STUB_LOG=$HOME/iso-task.log"
            " dshd run --name iso-task -- headless 'start a job'"
        )
        until_unit("iso-task", ActiveState="inactive")
        reaped(logged("iso-task.log", "background"))
        receipt("iso-b", "P1.6")

    # catches: a failed start retried, or reported as anything but
    # start-failed with the unit failed
    with subtest("P1.7 failures"):
        code, err = attempt("env DSH_STUB_MODE=exit-1 dshd run --bg --json --name early -- web")
        assert code == 1 and "start-failed" in err, err
        assert unit("early", "NRestarts") == "0" and unit("early", "ActiveState") == "failed"
        run("mkdir ~/gone")
        run_bg("--name nocwd --cwd ~/gone -- web", "web")
        dshd("stop nocwd")
        run("rmdir ~/gone")
        code, err = attempt("dshd start --json nocwd")
        assert code == 1 and "start-failed" in err, err
        assert unit("nocwd", "ExecMainStatus") == "78" and unit("nocwd", "NRestarts") == "0"
        run_bg("--name crashy -- web", "crash-after-ready")
        until_unit("crashy", ActiveState="failed", Result="start-limit-hit")
        report = get("crashy")
        assert report["state"] == "failed" and report["result"] == "start-limit-hit", report
        others = fixtures()
        code, err = attempt(f"env DSH_STUB_MODE=web dshd run --bg --json --name collide -- web --port {HELD_PORT}")
        assert code == 1 and "start-failed" in err, err
        assert unit("collide", "NRestarts") == "0" and fixtures() == others
        # AssertPathExists: a unit without an instance directory never runs.
        machine.fail(alice("systemctl --user start dshd@ghost.service"))
        assert unit("ghost", "AssertResult") == "no"
        # The profile is live: one that now selects another transport is
        # refused before dsh runs, never served as the recorded one.
        drift = "~/.dsh/profiles/drift"

        def drift_selects(bundles: list[str]) -> None:
            """Writes the drift profile's package.json with `bundles`."""
            manifest = json.dumps({"dsh": {"profile": {"bundles": bundles}}})
            run(f"mkdir -p {drift} && printf '%s\\n' {shlex.quote(manifest)} > {drift}/package.json")

        drift_selects(PROFILES["web"])
        run_bg("--name drift -- drift", "web", env="DSH_STUB_LOG=$HOME/drift.log")
        dshd("stop drift")
        drift_selects(PROFILES["acp"])
        code, err = attempt("dshd start --json drift")
        assert code == 1 and "start-failed" in err and "now selects transport acp" in err, err
        assert unit("drift", "ExecMainStatus") == "78" and unit("drift", "NRestarts") == "0"
        assert run("grep -c '^mode ' ~/drift.log").strip() == "1", "dsh never ran for the refused start"
        code, err = attempt("env DSH_STUB_MODE=web dshd run --bg --json --name drift -- drift")
        assert code == 4 and "exists-different-recipe" in err and "transport" in err, err
        receipt("crashy", "P1.7")

    # catches: creating, starting or restarting another instance of a
    # profile writing into that profile, which a running instance boots from
    with subtest("P2.2 no writes to profiles"):
        profiles = snapshot("~alice/.dsh/profiles")
        run_bg("--name p22 -- web", "web")
        dshd("restart p22")
        run_bg("--name p22-acp -- acp", "acp")
        assert snapshot("~alice/.dsh/profiles") == profiles
        receipt("web-1", "P2.2")

    # catches: the lifecycle writing anything in the DSH home, such as a
    # profile link, a copy or a stamp, at any verb; or rm leaving a GC root,
    # a drop-in or a wants link behind. The stub writes nothing there when
    # it boots, so the home must stay as it was, entry for entry.
    with subtest("P2.4 dsh home untouched"):
        home = "~alice/.dsh"
        run("mkdir -p ~/.dsh/sessions ~/.dsh/storages/web"
            " && printf x > ~/.dsh/sessions/p24.jsonl"
            " && printf '{}' > ~/.dsh/storages/web/state.json"
            " && printf 'k: v\\n' > ~/.dsh/.credentials.yaml")
        before = snapshot(home)
        run_bg("--name p24 -- web", "web")
        assert snapshot(home) == before, "run"
        dshd("enable p24")
        machine.succeed("test -L ~alice/.config/systemd/user/dshd.target.wants/dshd@p24.service")
        assert "dshd@p24.service" in run("systemctl --user show --property=Wants --value dshd.target")
        receipt("p24", "P2.4")
        for verb in ("restart", "stop", "start", "stop", "rm"):
            dshd(f"{verb} p24")
            assert snapshot(home) == before, verb
        machine.fail(f"test -e {STATE}/p24")
        # Nix ignores dangling indirect links; query the collector's live roots.
        machine.fail("nix-store --gc --print-roots | grep -F '/dshd/p24/'")
        machine.fail("find ~alice/.config/systemd/user -name 'dshd@p24*' | grep -q .")
        assert "dshd@p24.service" not in run("systemctl --user show --property=Wants --value dshd.target")

    # catches: a stop intent never written or never read, a systemctl stop
    # recorded as dshd's, a task's own end recorded as a stop, or an ended
    # task whose totals and end time vanish once the manager unloads its unit
    with subtest("P2.5 stop reasons"):
        run_bg("--name why -- web", "web")
        dshd("stop why")
        assert get("why")["lastRun"]["ending"] == "stopped"
        dshd("start why")
        run("systemctl --user stop dshd@why.service")
        assert get("why")["lastRun"]["ending"] == "stopped-without-intent"
        run("env DSH_STUB_MODE=headless dshd run --name done -- headless 'finish the task'")
        done = get("done")
        assert done["lastRun"]["ending"] == "exited"
        assert done["since"] is not None, done
        consumed = done["lastRun"]["resources"]
        for key in ("cpuUsageNSec", "memoryPeak", "ioReadBytes", "ioWriteBytes"):
            assert isinstance(consumed[key], int) and consumed[key] >= 0, consumed
        assert consumed["cpuUsageNSec"] > 0 and consumed["memoryPeak"] > 0, consumed
        assert consumed["memoryCurrent"] is None and consumed["tasksCurrent"] is None, consumed
        receipt("why", "P2.5")

    # catches: a stop by invocation id that stops a newer run of the same
    # name, whether systemd restarted the instance after a crash or dshd
    # started it again, that refuses or misreports the run it names, that
    # leaves a stop intent behind when it stopped nothing, or that compares
    # and stops without the instance lock
    with subtest("stop-invocation"):
        first = run_bg("--name fence -- web", "web")["invocationId"]
        assert first and first == unit("fence", "InvocationID"), first
        # dsh dies without a stop request: the runner exits 75, and systemd
        # restarts the instance as a new invocation after RestartSec.
        run(f"kill -KILL {child('fence')}")
        machine.wait_until_succeeds(
            alice(
                f'test "$(systemctl --user show --property=InvocationID --value dshd@fence.service)" != {first}'
                " && systemctl --user is-active dshd@fence.service"
            ),
            timeout=WAIT,
        )
        second = unit("fence", "InvocationID")

        def refused(stale: str, held: str) -> None:
            """A stop by `stale` stops nothing while the unit runs `held`."""
            code, err = attempt(f"dshd stop --json --invocation {stale} fence")
            error = json.loads(err)["error"]
            assert code == 5 and error["kind"] == "not-running", err
            assert error["message"] == f"fence is not running invocation {stale}: it runs {held}", err
            assert error["hint"] == "dshd get fence", err
            report = get("fence")
            assert report["state"] == "running" and report["invocationId"] == held, report
            run('test ! -e "$XDG_RUNTIME_DIR/dshd/fence/stop-intent"')

        refused(first, second)
        stopped = dshd(f"stop --invocation {second} fence")
        assert stopped["outcome"] == "stopped", stopped
        assert stopped["lastRun"]["invocationId"] == second, stopped
        assert stopped["lastRun"]["ending"] == "stopped", stopped
        again = dshd(f"stop --invocation {second.upper()} fence")
        assert again["outcome"] == "already-stopped", again
        code, err = attempt(f"dshd stop --json --invocation {first} fence")
        assert code == 5 and "its last run was " + second in err, err
        assert dshd("start fence")["outcome"] == "started"
        third = unit("fence", "InvocationID")
        assert third not in (first, second), third
        refused(second, third)
        receipt("fence", "stop-invocation")
        # The comparison and the stop wait for the instance lock, which
        # dshd's own starts take: while another holder keeps it, the stop
        # by invocation does not return.
        run(f"rm -f ~/fence-held ~/fence-released; (flock {STATE}/fence/lock sh -c"
            " 'touch ~/fence-held; sleep 5; touch ~/fence-released' </dev/null >/dev/null 2>&1 &)")
        machine.wait_until_succeeds(alice("test -e ~/fence-held"), timeout=WAIT)
        stopped = dshd(f"stop --invocation {third} fence")
        assert stopped["outcome"] == "stopped", stopped
        run("test -e ~/fence-released")

    # catches: DSH_HOME of the starting shell not reaching dsh or get, or a
    # profile that does not exist yet made by dshd rather than by dsh: in a
    # fresh home, only the manifest dsh materialises appears
    with subtest("P2.6 dsh home selection"):
        work = machine.succeed("echo ~alice/.dsh-work").strip()
        run_bg("--name homed -- web", "web", env=f"DSH_HOME={work}")
        assert environ(child("homed"))["DSH_HOME"] == work
        assert get("homed")["profile"] == {"name": "web", "dshHome": work, "patches": []}
        made = machine.succeed(f"find {work} -mindepth 1 -printf '%P\\n' | sort").split()
        assert made == ["profiles", "profiles/web", "profiles/web/package.json"], made
        materialised = json.loads(machine.succeed(f"cat {work}/profiles/web/package.json"))
        assert materialised["dsh"]["profile"]["bundles"] == ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"]
        assert native_args(child("homed")) == ["web", "--no-open", "--port", "0"]
        receipt("homed", "P2.6")

    # catches: dsh started outside the task directory, the label lost, or
    # web flags injected into an ACP instance
    with subtest("P3.4 cwd and agency label"):
        task = machine.succeed("echo ~alice/task").strip()
        agency = machine.succeed("echo ~alice/agency").strip()
        run(f"mkdir -p {task} {agency}")
        run_bg(f"--name task --cwd {task} --label agency={agency} -- acp", "acp")
        pid, report = child("task"), get("task")
        assert machine.succeed(f"readlink /proc/{pid}/cwd").strip() == task
        assert report["cwd"] == task and report["labels"] == {"agency": agency}, report
        args = native_args(pid)
        assert args == report["args"] and "--no-open" not in args and "--port" not in args, args
        receipt("task", "P3.4")

    # catches: a run's --patch overlay lost from the argv dsh gets, dropped
    # at the next start, or missing from get and ls
    with subtest("run-patch per-run overlay"):
        run_bg("--name overlaid -- acp --patch ./extra.yml --patch=/etc/extra.yml", "acp")
        overlays = ["./extra.yml", "/etc/extra.yml"]
        want = ["acp", "--patch", "./extra.yml", "--patch=/etc/extra.yml"]
        assert native_args(child("overlaid")) == want, native_args(child("overlaid"))
        assert get("overlaid")["profile"]["patches"] == overlays, get("overlaid")["profile"]
        entry = next(item for item in listing() if item["name"] == "overlaid")
        assert (entry["profile"], entry["profilePatches"]) == ("acp", overlays), entry
        dshd("stop overlaid")
        assert dshd("start overlaid")["outcome"] == "started"
        assert native_args(child("overlaid")) == want, native_args(child("overlaid"))
        receipt("overlaid", "run-patch")
        dshd("stop overlaid")

    # catches: dshd keeping a store or log of its own beside the record,
    # including a new file or one that grows on each inspection
    with subtest("P4.1 native inspection"):
        before = snapshot(f"{STATE}/web-1")
        for _ in range(10):
            get("web-1")
            listing()
            run("dshd read web-1")
            run("dshd attach web-1")
        assert snapshot(f"{STATE}/web-1") == before
        entries = set(run(f"ls -A {STATE}/web-1").splitlines())
        assert entries <= {"instance.json", "last-run.json", "current", "booted", "web-port", "lock"}, entries
        # NixOS's default DefaultIOAccounting=yes gives the user manager the io
        # controller too, so every counter is live.
        resources = get("web-1")["resources"]
        for key in ("cpuUsageNSec", "memoryCurrent", "memoryPeak", "tasksCurrent", "ioReadBytes", "ioWriteBytes"):
            assert isinstance(resources[key], int) and resources[key] >= 0, resources
        assert resources["memoryCurrent"] > 0 and resources["tasksCurrent"] > 0, resources
        receipt("web-1", "P4.1")

    # catches: a captured secret or the web token in unit text, manager
    # properties, get, ls, read or the journal
    with subtest("P4.2 secrets"):
        secret = "canary-7c1e"
        run_bg("--name sec -- web", "web", env=f"SECRET_PLANT={secret} SCRUB_PLANT=scrub-x")
        assert "token=stub" in run("dshd attach --print-url sec")
        shown = [
            run("systemctl --user cat dshd@sec.service"),
            run("systemctl --user show dshd@sec.service"),
            run("dshd get --json sec"),
            run("dshd ls --json"),
            run("dshd read sec"),
            machine.succeed("journalctl --no-pager -o cat"),
        ]
        assert [text for text in shown if secret in text or "token=stub" in text or "scrub-x" in text] == []
        logged_url = machine.succeed("journalctl --no-pager -o cat _SYSTEMD_USER_UNIT=dshd@sec.service")
        assert "REDACTED" in logged_url, logged_url
        record = f"{STATE}/sec/instance.json"
        assert machine.succeed(f"stat -c %a {record}").strip() == "600"
        machine.succeed(f"grep -q {secret} {record}")
        machine.fail(f"grep -q scrub-x {record}")
        assert "SCRUB_PLANT" not in environ(child("sec"))
        assert "SECRET_PLANT" in get("sec")["environmentNames"]
        receipt("sec", "P4.2")

    # catches: the runner calling nix to start an instance
    with subtest("P4.3 no nix at runtime"):
        run_bg("--name pinned -- web", "web")
        dshd("stop pinned")
        path = run("systemctl --user show-environment | sed -n 's/^PATH=//p'").strip()
        run(f"systemctl --user set-environment PATH={NIX_MASK}/bin:{path}")
        dshd("start pinned")
        run(f"systemctl --user set-environment PATH={shlex.quote(path)}")
        assert get("pinned")["state"] == "running"
        machine.fail(alice('test -e "$XDG_RUNTIME_DIR/nix-called"'))
        receipt("pinned", "P4.3")

    # catches: Ctrl+C at the terminal of a foreground run reaching the
    # instance instead of only the command, and wait not mirroring the code
    with subtest("foreground run: Ctrl+C stays with the command"):
        command = "echo $$ > ~/fg.pid; exec env DSH_STUB_MODE=headless DSH_STUB_DELAY=40 dshd run --name fg -- headless 'a slow task'"
        run(f"setsid -f sh -c {shlex.quote(command)} >~/fg.out 2>&1 </dev/null")
        machine.wait_until_succeeds(alice("test -s ~/fg.pid"), timeout=WAIT)
        cli_pid = int(run("cat ~/fg.pid"))
        until_unit("fg", ActiveState="active")
        machine.wait_until_succeeds(f"pgrep -P {cli_pid} -x journalctl", timeout=WAIT)
        # A real journalctl remains attached; only the check-in deadline can
        # make wait return while the admitted task is still running.
        code, _ = machine.execute(alice("dshd wait --text --timeout 2 fg"), timeout=WAIT)
        assert code == 124 and get("fg")["state"] == "running", code
        machine.succeed(f"kill -INT -- -{cli_pid}")
        reaped(cli_pid)
        report = get("fg")
        assert report["state"] == "running" and unit("fg", "NRestarts") == "0", report
        code, _ = machine.execute(alice("dshd wait --text fg"), timeout=WAIT)
        ended = get("fg")["lastRun"]
        assert ended["ending"] == "exited" and code == ended["child"]["exitCode"], (code, ended)
        receipt("fg", "run-interrupt")

    # catches: what dsh printed just before it exited lost from the
    # foreground run's output
    with subtest("foreground run: output before the exit"):
        out = run("env DSH_STUB_MODE=headless dshd run --text --name quick -- headless printed-before-exit")
        assert "printed-before-exit" in out, out
        assert get("quick")["lastRun"]["ending"] == "exited"
        receipt("quick", "run-output")

    # catches: --head showing the newest lines, a head that loses the run's
    # first line, or complete claimed for a head that newer lines follow, on
    # the real journal of an ended headless run
    with subtest("read --head"):
        whole = json.loads(run("dshd read --json quick"))
        assert whole["complete"] is True and len(whole["lines"]) >= 2, whole
        assert whole["lines"][0] == "dshd-run: run started", whole
        first = json.loads(run("dshd read --json --head 1 quick"))
        assert first["lines"] == whole["lines"][:1] and first["complete"] is False, first
        assert first["invocationId"] == whole["invocationId"] == get("quick")["lastRun"]["invocationId"], first
        everything = json.loads(run(f"dshd read --json --head {len(whole['lines'])} quick"))
        assert everything == whole, everything
        code, note = attempt("dshd read --head 1 quick")
        assert code == 0 and "newer output follows" in note, note
        receipt("quick", "read-head")

    # catches: a self-restart that loses the instance or resends a prompt:
    # the agent's own `dshd restart` ends with it, systemd completes the job
    with subtest("self-restart"):
        once = "test -e ~/selfr.done || { touch ~/selfr.done; dshd restart selfr; }"
        run_bg(
            "--name selfr -- web",
            "web",
            env=f"DSH_STUB_LOG=$HOME/selfr.log DSH_STUB_RUN_AFTER_READY={shlex.quote(once)}",
        )
        # Two runs of the stub: the first, which restarted itself, and the
        # one systemd started to complete that restart.
        machine.wait_until_succeeds(
            alice(
                "test -e ~/selfr.done"
                " && test \"$(grep -c '^mode [0-9]* web$' ~/selfr.log)\" = 2"
                " && systemctl --user is-active dshd@selfr.service"
            ),
            timeout=WAIT,
        )
        report = get("selfr")
        assert report["state"] == "running" and report["invocationId"] == unit("selfr", "InvocationID"), report
        assert run("grep -c '^mode [0-9]* web$' ~/selfr.log").strip() == "2"
        receipt("selfr", "self-restart")

    # catches: the linger warning lost, get hiding linger, or dshd enabling
    # linger itself
    with subtest("linger off warns"):
        _, made = over_ssh("DSH_STUB_MODE=web dshd run --bg -- web 2>&1", user="bob")
        assert "linger" in made, made
        _, shown = over_ssh("dshd get --text web-1", user="bob")
        assert "linger: no" in shown, shown
        _, out = over_ssh("dshd get --json web-1", user="bob")
        report = json.loads(out)
        assert report["linger"] is False, report
        machine.fail("test -e /var/lib/systemd/linger/bob")
        receipt("web-1", "linger-off", report)

    finish([
        "read-head",
        "P1.1", "P1.2", "P1.3", "P1.5", "P1.6", "P1.7", "P2.2", "P2.4", "P2.5", "P2.6", "P3.4",
        "P4.1", "P4.2", "P4.3", "linger-off", "run-interrupt", "run-output", "run-patch", "self-restart",
        "stop-invocation",
    ])
  '';
}
