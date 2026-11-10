# VM: switches between specialisations with instances running (P2.1, P2.3,
# P2.7). r2 replaces release r1 with r2 and changes the template text; the
# switch back to the base configuration is the rollback; restart is the
# negative control, the same kind of change with restartIfChanged =
# mkForce true, which must restart the instances.
# catches: a switch, rollback, daemon-reload or daemon-reexec restarting,
# stopping or re-releasing a running instance; garbage collection freeing a
# release an instance holds or keeping one none holds; dshd touching other
# units.
{ self, pkgs }:
let
  inherit (pkgs) lib;
  vm = import ./vm-node.nix { inherit self pkgs; };

  # A release outside the system closure: only the instance that uses it
  # keeps it from the garbage collector.
  gcProbe = self.lib.mkRelease {
    inherit pkgs;
    dsh = vm.stub "gc";
  };
in
pkgs.testers.runNixOSTest {
  name = "dshd-switch";
  globalTimeout = 1800;
  nodes.machine = {
    imports = [ vm.node ];
    virtualisation.additionalPaths = [ gcProbe ];
    specialisation = {
      r2.configuration.dshd = {
        releases = lib.mkForce { r2.dsh = vm.stub "2"; };
        startupTimeout = 151;
      };
      removed.configuration.dshd.enable = lib.mkForce false;
      stopremoval.configuration.systemd.user.services."dshd@".unitConfig.X-StopOnRemoval =
        lib.mkForce true;
      restart.configuration = {
        systemd.user.services."dshd@".restartIfChanged = lib.mkForce true;
        dshd.startupTimeout = 152;
      };
    };
  };
  testScript = vm.prelude + ''
    GC_PROBE = "${gcProbe}"

    start_all()
    setup()
    toplevel = machine.succeed("readlink -f /run/current-system").strip()


    def switch(specialisation: str | None) -> list[str]:
        """switch-to-configuration test into a specialisation, or back to the
        base configuration: its output."""
        system = toplevel if specialisation is None else f"{toplevel}/specialisation/{specialisation}"
        return machine.succeed(f"{system}/bin/switch-to-configuration test 2>&1").splitlines()


    def touched(lines: list[str], name: str) -> list[str]:
        """The lines of a switch's output that stop or restart dshd@NAME.service:
        one predicate for the claim and for its negative control."""
        service = f"dshd@{name}.service"
        return [line for line in lines if line.startswith(("stopping", "restarting")) and service in line]


    def identity(name: str) -> tuple[str, str]:
        return unit(name, "MainPID"), unit(name, "InvocationID")


    run_bg("--name a -- web", "web")
    run_bg(f"--name g --release {GC_PROBE} -- web", "web")
    machine.succeed(f"ln -s {GC_PROBE} /etc/dshd/releases/ephemeral")
    run_bg("--name g2 --release ephemeral -- web", "web")
    unmanaged = unmanaged_dsh()
    bystanders = fixtures()
    r1 = get("a")["release"]["booted"]
    copy = snapshot(f"{STATE}/a/profile")
    args = native_args(child("a"))

    def live_roots(name: str, target: str, links: tuple[str, ...] = ("current", "booted")) -> None:
        """Both indirect roots are registered at their final, live paths."""
        directory = machine.succeed(f"readlink -f {STATE}/{name}").strip()
        # One root per line: "ROOT" -> TARGET.
        roots = machine.succeed("nix-store --gc --print-roots").splitlines()
        assert all(f'"{directory}/{link}" -> {target}' in roots for link in links), roots


    live_roots("g", GC_PROBE)
    live_roots("g2", GC_PROBE)

    # catches: a switch, a reload or a reexec of the user manager restarting A,
    # or a new release or profile edit reaching it
    with subtest("P2.1 release isolation"):
        before = identity("a")
        run("printf '# edited: true\\n' >> ~/.dsh/profiles/web/cordis.patch.yml")
        lines = switch("r2")
        for name in ("a", "g", "g2"):
            assert touched(lines, name) == [], lines
        run("systemctl --user daemon-reload && systemctl --user daemon-reexec")
        machine.wait_until_succeeds(alice("systemctl --user is-active dshd@a.service"))
        assert identity("a") == before
        pid, cgroup = child("a"), unit("a", "ControlGroup")
        assert machine.succeed(f"cat /proc/{pid}/cgroup").strip().endswith(cgroup)
        run_bg("--name b -- web", "web")
        a, b = get("a")["release"], get("b")["release"]
        assert a["current"] == a["booted"] == r1 and b["booted"] != r1, (a, b)
        assert snapshot(f"{STATE}/a/profile") == copy and native_args(child("a")) == args
        machine.succeed(f"grep -q edited {STATE}/b/profile/cordis.patch.yml")
        machine.fail(f"grep -q edited {STATE}/a/profile/cordis.patch.yml")
        receipt("a", "P2.1")
        receipt("b", "P2.1")

    # catches: the collector freeing a release an instance holds, update or
    # restart misreporting the release, update listing no closure
    # difference, rm keeping a release alive
    with subtest("P2.3 gc"):
        machine.succeed("nix-collect-garbage -d")
        machine.succeed(f"nix-store --check-validity {GC_PROBE}")
        live_roots("a", r1)
        dshd("restart a")
        a = get("a")["release"]
        assert a["booted"] == r1 and a["state"] == "outdated", a
        # JSON carries the closure difference among the warnings.
        updated = dshd("update a")
        said = updated["warnings"]
        assert updated["outcome"] == "updated", updated
        assert not any(line.startswith("cannot compare") for line in said), said
        assert any("stub-dsh" in line for line in said), said
        assert get("a")["release"]["state"] == "stale"
        dshd("restart a")
        a = get("a")["release"]
        assert a["state"] == "current" and a["booted"] == get("b")["release"]["booted"], a
        receipt("g", "P2.3")
        dshd("stop g")
        dshd("rm g")
        machine.succeed("nix-collect-garbage")
        # g2 retains the same release after its sibling is removed.
        machine.succeed(f"nix-store --check-validity {GC_PROBE}")
        live_roots("g2", GC_PROBE)
        machine.fail(f"test -e {STATE}/g")
        machine.fail("find /nix/var/nix/gcroots/auto -lname '*/dshd/g/*' | grep -q .")
        # Move only current: booted is now the only root for GC_PROBE.
        replacement = get("a")["release"]["current"]
        machine.succeed(f"ln -sfn {replacement} /etc/dshd/releases/ephemeral")
        dshd("update g2")
        assert get("g2")["release"]["state"] == "stale"
        live_roots("g2", GC_PROBE, ("booted",))
        live_roots("g2", replacement, ("current",))
        machine.succeed("nix-collect-garbage")
        machine.succeed(f"nix-store --check-validity {GC_PROBE}")
        dshd("stop g2")
        live_roots("g2", replacement)
        assert get("g2")["release"]["state"] == "current"
        machine.succeed("nix-collect-garbage")
        machine.fail(f"nix-store --check-validity {GC_PROBE}")
        dshd("rm g2")
        machine.fail("nix-store --gc --print-roots | grep -F '/dshd/g2/'")
        receipt("a", "P2.3")

    # catches: a switch, start, update, stop or rm touching a fixture unit
    # or an unmanaged dsh, or ls showing them
    with subtest("P2.7 bystanders"):
        assert fixtures() == bystanders
        machine.succeed(f"kill -0 {unmanaged}")
        assert sorted(entry["name"] for entry in listing()) == ["a", "b"]
        receipt("a", "P2.7")

    # catches: a rollback restarting a retained instance, or freeing the
    # release an instance booted, or get misreporting that release
    with subtest("P2.3 rollback"):
        before = {name: identity(name) for name in ("a", "b")}
        booted = get("b")["release"]["booted"]
        lines = switch(None)
        for name in ("a", "b"):
            assert touched(lines, name) == [], lines
        assert {name: identity(name) for name in ("a", "b")} == before
        machine.succeed("nix-collect-garbage -d")
        live_roots("b", booted)
        b = get("b")["release"]
        assert b["booted"] == booted and b["state"] == "outdated", b
        receipt("b", "P2.3")

    # catches: a stopped enabled instance omitted by target activation, a
    # disabled instance started, a failed enabled instance not re-armed, or
    # enable linking the template's store path into the user's unit
    # directory, which shadows every later template and hides dshd.target
    # from the switch
    with subtest("switch starts enabled instances only"):
        for name in ("auto", "still"):
            run_bg(f"--name {name} -- web", "web")
        dshd("enable auto")
        assert unit("auto", "FragmentPath") == "/etc/systemd/user/dshd@.service"
        assert unit("auto", "WantedBy") == "dshd.target"
        for name in ("auto", "still"):
            dshd(f"stop {name}")
        switch("r2")
        until_unit("auto", ActiveState="active")
        assert unit("still", "ActiveState") != "active"
        machine.succeed(f"kill -KILL {unit('auto', 'MainPID')}")
        until_unit("auto", ActiveState="failed")
        switch(None)
        until_unit("auto", ActiveState="active")
        assert unit("still", "ActiveState") != "active"
        receipt("auto", "autostart")

    # catches: X-StopOnRemoval missing or in the wrong section, so dropping
    # the module ends live instances; its inverse must actually stop them
    with subtest("module removal preserves running instances"):
        before = {name: identity(name) for name in ("a", "b", "auto")}
        lines = switch("removed")
        for name in before:
            assert touched(lines, name) == [], lines
        assert {name: identity(name) for name in before} == before
        switch(None)
        assert {name: identity(name) for name in before} == before
        receipt("a", "removal")
        switch("stopremoval")
        lines = switch("removed")
        assert touched(lines, "a") != [], lines
        assert identity("a") != before["a"]
        switch(None)
        dshd("start a")
        dshd("start b")
        receipt("a", "removal-control")

    # catches: the no-restart assertions passing vacuously: the same kind of
    # switch with restartIfChanged forced true must restart A, as the same
    # predicate sees it
    with subtest("negative control: restartIfChanged forced true restarts"):
        before = identity("a")
        lines = switch("restart")
        assert touched(lines, "a") != [], lines
        machine.wait_until_succeeds(alice("systemctl --user is-active dshd@a.service"))
        assert identity("a") != before
        assert fixtures() == bystanders
        receipt("a", "control")

    finish(["P2.1", "P2.3", "P2.7", "autostart", "removal", "removal-control", "control"])
  '';
}
