# VM: the web and ACP transports with the stub dsh (P1.4, P3.1-P3.3)
# and what `dshd read` reports of the journal.
# catches: a changed web origin or port, a stale token, an ACP session or
# held permission lost with its controller, an attach that starts or
# spawns anything, get read from anything but the record and the user
# manager, and a journal cut short presented as whole.
{ self, pkgs }:
let
  vm = import ./vm-node.nix { inherit self pkgs; };
in
pkgs.testers.runNixOSTest {
  name = "dshd-transport";
  globalTimeout = 1800;
  nodes.machine = vm.node;
  testScript = vm.prelude + ''
    def detached(name: str) -> None:
        """Waits until the relay of NAME reports no controller. Nothing but
        the relay's own report says when it has seen its controller go."""
        machine.wait_until_succeeds(
            alice(f"dshd get --json {name} | jq -e '.acp.attached == false'"), timeout=WAIT
        )


    def handover(mode: str, first_over_ssh: bool) -> None:
        """A controller opens a session, prompts for a permission and ends
        (`mode`); a second one resumes the session, gets the held request,
        answers it and prompts again. dsh is the same process throughout."""
        pid, invocation = child("agent"), unit("agent", "InvocationID")
        command = f"acp-client agent new {mode}"
        if first_over_ssh:
            session, output = over_ssh(command)
            machine.wait_until_fails(f"loginctl show-session {session}", timeout=WAIT)
        else:
            output = run(command)
        first = json.loads(output)
        detached("agent")
        probe = get("agent")["acp"]
        held = [s for s in probe["heldSessions"] if s["sessionId"] == first["sessionId"]]
        assert probe["pendingPermissions"] == 1 and held and not held[0]["bound"], probe
        second = json.loads(run(f"acp-client agent resume {first['sessionId']}"))
        methods = [frame.get("method") for frame in second["frames"]]
        assert "session/request_permission" in methods and "session/update" in methods, methods
        assert (child("agent"), unit("agent", "InvocationID")) == (pid, invocation)
        assert unit("agent", "NRestarts") == "0"


    start_all()
    setup()
    run_bg("--name site --label team=blue -- web", "web")
    run_bg("--name agent -- acp", "acp")

    # catches: an origin or pinned port that changes, or an expired token
    # returned from a preceding invocation
    with subtest("P3.1 web url survives"):
        url = run("dshd attach --print-url site").strip()
        web = dshd("attach site")
        assert url.startswith(web["origin"]) and "token=stub" in url, (url, web)
        port = get("site")["web"]["port"]
        assert web["port"] == port and run(f"cat {STATE}/site/web-port").strip() == str(port)
        invocation = unit("site", "InvocationID")
        assert dshd("restart site")["outcome"] == "restarted"
        assert unit("site", "InvocationID") != invocation
        assert get("site")["web"]["port"] == port
        fresh = run("dshd attach --print-url site").strip()
        assert fresh != url and "token=stub" in fresh, (url, fresh)
        assert dshd("attach site")["origin"] == web["origin"]
        assert fresh.partition("?")[0] == url.partition("?")[0], (url, fresh)
        receipt("site", "P3.1")

    # catches: a cleanly detached controller taking its session or the held
    # permission with it
    with subtest("P3.1 acp handover"):
        handover("clean", first_over_ssh=False)
        receipt("agent", "P3.1")

    # catches: a controller killed mid-turn, in an SSH session that then ends,
    # taking dsh, the session or the held permission down
    with subtest("P1.4 controller churn"):
        handover("abrupt", first_over_ssh=True)
        receipt("agent", "P1.4")

    # catches: attaches that spawn a process, change the instance's identity
    # or lose its labels
    with subtest("P3.3 one unit many attaches"):
        pid, runner = child("site"), unit("site", "MainPID")
        before = (unit("site", "InvocationID"), get("site")["id"])
        for _ in range(2):
            run("dshd attach site")
        run("acp-client agent new clean")
        assert (child("site"), unit("site", "MainPID")) == (pid, runner)
        assert (unit("site", "InvocationID"), get("site")["id"]) == before
        assert get("site")["labels"] == {"team": "blue"}
        cgroup = unit("site", "ControlGroup")
        assert machine.succeed(f"cat /proc/{pid}/cgroup").strip().endswith(cgroup)
        receipt("site", "P3.3")

    # catches: attach starting a stopped instance, or answering through the
    # wrong transport
    with subtest("P3.2 attach never starts"):
        dshd("stop site")
        listed = run("systemctl --user list-units --all --plain --no-legend 'dshd@*'")
        pid = child("agent")
        code, err = attempt("dshd attach --json site")
        assert code == 5 and "not-running" in err, err
        code, err = attempt("dshd attach --json --via web agent")
        assert code == 6 and "unsupported-transport" in err, err
        assert run("systemctl --user list-units --all --plain --no-legend 'dshd@*'") == listed
        assert unit("site", "ActiveState") != "active" and child("agent") == pid
        receipt("agent", "P3.2")

    # catches: read hiding that an ACP instance's stdout is its controller's
    # channel, or presenting a journal cut short by rotation as the whole run
    with subtest("read completeness"):
        note = machine.succeed(alice("dshd read agent 2>&1 >/dev/null"))
        assert "ACP" in note, note
        assert json.loads(run("dshd read --json site"))["complete"] is True
        machine.succeed("journalctl --rotate --vacuum-size=1")
        assert json.loads(run("dshd read --json site"))["complete"] is False
        receipt("site", "read")

    finish(["P1.4", "P3.1", "P3.2", "P3.3", "read"])
  '';
}
