# Evaluates the NixOS module and asserts, at evaluation time: the exact line
# sequence of the rendered dshd@.service (so no Environment=, store path or
# RestartPreventExitStatus can appear), dshd.target, the /etc/dshd/releases
# registry with the single-release default, and the release-name grammar
# against tests/fixtures/instance-names.json. The build then reads what
# lib.mkRelease rendered: the links and release.json.
# catches: a unit line added, dropped or changed (a store path, Environment=,
# a restart key), a wrong registry or default, and an accepted bad name.
{ self, pkgs }:
let
  inherit (pkgs) lib;
  inherit (builtins) toJSON;

  stub = import ./stub-dsh.nix {
    inherit pkgs;
    version = "1";
  };

  evaluate =
    dshd:
    (pkgs.nixos [
      self.nixosModules.default
      {
        dshd = {
          enable = true;
        }
        // dshd;
      }
    ]).config;

  failedDshdAssertions =
    config:
    map (a: a.message) (
      lib.filter (a: !a.assertion && lib.hasPrefix "dshd" a.message) config.assertions
    );

  # One release, no defaultRelease: the release is the default.
  single = evaluate { releases.stub.dsh = stub; };
  unitLines = lib.filter (line: line != "") (
    lib.splitString "\n" single.systemd.user.units."dshd@.service".text
  );
  expectedUnitLines = [
    "[Unit]"
    "AssertPathExists=%S/dshd/%i/current/bin/dshd-run"
    "Description=dshd instance %i"
    "StartLimitBurst=5"
    "StartLimitIntervalSec=1800"
    "X-StopOnRemoval=false"
    "[Service]"
    "X-RestartIfChanged=false"
    "ExecStart=%S/dshd/%i/current/bin/dshd-run run %i"
    "ExecStopPost=%S/dshd/%i/booted/bin/dshd-run poststop %i"
    "IOAccounting=true"
    "KillMode=mixed"
    "MemoryAccounting=true"
    "NotifyAccess=main"
    "OOMPolicy=continue"
    "Restart=no"
    "RestartForceExitStatus=75"
    "RestartSec=5s"
    "RuntimeDirectory=dshd/%i"
    "RuntimeDirectoryMode=0700"
    "SyslogIdentifier=dshd@%i"
    "TasksAccounting=true"
    "TimeoutStartSec=150"
    "TimeoutStopSec=30s"
    "Type=notify"
    "WorkingDirectory=%S/dshd/%i"
  ];

  registry = lib.filterAttrs (path: _: lib.hasPrefix "dshd/" path) single.environment.etc;
  defaultRelease = registry."dshd/releases/default".source;

  # Every fixture name as a release, plus the reserved `default`, with a
  # defaultRelease that names none of them.
  names = lib.importJSON ../tests/fixtures/instance-names.json;
  grammar = failedDshdAssertions (evaluate {
    releases = lib.genAttrs (names.valid ++ names.invalid ++ [ "default" ]) (_: {
      dsh = stub;
    });
    defaultRelease = "absent";
  });
  rejected = name: lib.any (lib.hasInfix (toJSON name)) grammar;

  several = failedDshdAssertions (evaluate {
    releases.one.dsh = stub;
    releases.two.dsh = stub;
  });

  merged = self.lib.mkRelease {
    inherit pkgs;
    dsh = stub;
    environment.EXTRA = "1";
    unsetEnvironment = [ "PLANTED_SECRET" ];
  };
  overridden = self.lib.mkRelease {
    inherit pkgs;
    dsh = stub;
    environment.DSH_PERMISSION_MODE = "workspace-write";
  };
  refuses =
    unsetEnvironment:
    !(builtins.tryEval
      (self.lib.mkRelease {
        inherit pkgs unsetEnvironment;
        dsh = stub;
      }).drvPath
    ).success;

  problems =
    lib.optional (
      unitLines != expectedUnitLines
    ) "dshd@.service is ${toJSON unitLines}, expected ${toJSON expectedUnitLines}"
    ++ lib.optional (
      single.systemd.user.targets.dshd.wantedBy != [ "default.target" ]
    ) "dshd.target is not wanted by default.target"
    ++ lib.optional (
      lib.attrNames registry != [
        "dshd/releases/default"
        "dshd/releases/stub"
      ]
    ) "the registry is ${toJSON (lib.attrNames registry)}"
    ++ lib.optional (
      "${defaultRelease}" != "${registry."dshd/releases/stub".source}"
    ) "dshd/releases/default is not the only release"
    ++ lib.optional (failedDshdAssertions single != [ ]) "a valid configuration fails its assertions"
    ++ map (name: "the valid release name ${toJSON name} is rejected") (lib.filter rejected names.valid)
    ++ map (name: "the release name ${toJSON name} is accepted") (
      lib.filter (name: !rejected name) (names.invalid ++ [ "default" ])
    )
    ++ lib.optional (
      !lib.any (lib.hasInfix "\"absent\"") grammar
    ) "a defaultRelease naming no release is accepted"
    ++ lib.optional (several == [ ]) "several releases without defaultRelease are accepted"
    ++ map (list: "lib.mkRelease accepts unsetEnvironment = ${toJSON list}") (
      lib.filter (list: !refuses list) [
        [ "" ]
        [ "A=B" ]
        [ "DSH_PERMISSION_MODE" ]
        [ "DSH_HOME" ]
        [ "PWD" ]
        [ "INVOCATION_ID" ]
        [ "JOURNAL_STREAM" ]
      ]
    );
in
assert lib.assertMsg (problems == [ ]) (lib.concatStringsSep "\n" problems);
pkgs.runCommand "dshd-module" { nativeBuildInputs = [ pkgs.jq ]; } ''
  test "$(readlink ${defaultRelease}/bin/dsh)" = ${stub}/bin/dsh
  test -x ${defaultRelease}/bin/dshd-run
  jq -e '. == {environment: {DSH_PERMISSION_MODE: "danger-full-access"}, unsetEnvironment: []}' \
    ${defaultRelease}/share/dshd/release.json
  jq -e '.environment == {DSH_PERMISSION_MODE: "danger-full-access", EXTRA: "1"}' \
    ${merged}/share/dshd/release.json
  jq -e '.unsetEnvironment == ["PLANTED_SECRET"]' ${merged}/share/dshd/release.json
  jq -e '.environment.DSH_PERMISSION_MODE == "workspace-write"' ${overridden}/share/dshd/release.json
  touch "$out"
''
