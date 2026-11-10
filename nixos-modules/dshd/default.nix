# Installs the static user-manager template dshd@.service, dshd.target, the
# release registry under /etc/dshd/releases and the dshd CLI. The rendered
# unit text holds specifiers and fixed values, and of the options only
# startupTimeout (TimeoutStartSec); no store path, user name, home path or
# port, so it is byte-stable across releases.
#
# The template turns restartIfChanged and X-StopOnRemoval off, which keeps
# every switch, rollback, upgrade or removal of this module away from running
# instances: an instance changes only when its user restarts it.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.dshd;
  inherit (import ../../lib { inherit lib; }) mkRelease;

  releaseNames = lib.attrNames cfg.releases;

  releases = lib.mapAttrs (
    _: release:
    mkRelease {
      inherit pkgs;
      inherit (release) dsh environment unsetEnvironment;
      dshd = cfg.package;
    }
  ) cfg.releases;

  # `defaultRelease`, or the only release when exactly one is declared.
  defaultName =
    if cfg.defaultRelease != null then
      cfg.defaultRelease
    else if lib.length releaseNames == 1 then
      lib.head releaseNames
    else
      null;

  # The instance-name grammar: a release name is joined into
  # /etc/dshd/releases/<name> and given to `dshd run --release`. A name
  # shaped like an instance id (a UUID) is refused, so names and ids never
  # share a spelling.
  nameGrammar = "[a-z0-9]([a-z0-9_-]{0,61}[a-z0-9])?";
  idShape = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
  validReleaseName =
    name:
    name != "default" && builtins.match nameGrammar name != null && builtins.match idShape name == null;

  # The restart policy: systemd restarts only the runner's exit 75, a ready
  # web or acp dsh that crashed, after a pause that lets a crash loop show,
  # and at most five starts in half an hour before the instance stays failed.
  crashedAfterReady = 75;
  restartPause = "5s";
  startLimitBurst = 5;
  startLimitIntervalSec = 1800;
  # The runner's time to stop dsh before KillMode=mixed kills what is left.
  stopTimeout = "30s";
in
{
  imports = [ ./options.nix ];

  config = lib.mkIf cfg.enable {
    assertions =
      map (name: {
        assertion = validReleaseName name;
        message = "dshd.releases: the name ${builtins.toJSON name} must match ^${nameGrammar}$, must not be \"default\" and must not be shaped like an instance id";
      }) releaseNames
      ++ [
        {
          assertion = cfg.defaultRelease == null || cfg.releases ? ${cfg.defaultRelease};
          message = "dshd.defaultRelease names ${builtins.toJSON cfg.defaultRelease}, which is not in dshd.releases";
        }
        {
          assertion = cfg.defaultRelease != null || lib.length releaseNames <= 1;
          message = "dshd.defaultRelease must name one of the releases when several are declared";
        }
      ];

    systemd.user.services."dshd@" = {
      description = "dshd instance %i";
      restartIfChanged = false;
      path = lib.mkForce [ ];
      environment = lib.mapAttrs (_: _: null) config.systemd.globalEnvironment;
      unitConfig = {
        AssertPathExists = "%S/dshd/%i/current/bin/dshd-run";
        StartLimitIntervalSec = startLimitIntervalSec;
        StartLimitBurst = startLimitBurst;
        X-StopOnRemoval = false;
      };
      serviceConfig = {
        MemoryAccounting = true;
        TasksAccounting = true;
        IOAccounting = true;
        Type = "notify";
        NotifyAccess = "main";
        WorkingDirectory = "%S/dshd/%i";
        ExecStart = "%S/dshd/%i/current/bin/dshd-run run %i";
        ExecStopPost = "%S/dshd/%i/booted/bin/dshd-run poststop %i";
        RuntimeDirectory = "dshd/%i";
        RuntimeDirectoryMode = "0700";
        Restart = "no";
        RestartForceExitStatus = crashedAfterReady;
        RestartSec = restartPause;
        TimeoutStartSec = cfg.startupTimeout;
        TimeoutStopSec = stopTimeout;
        KillMode = "mixed";
        OOMPolicy = "continue";
        SyslogIdentifier = "dshd@%i";
      };
    };

    systemd.user.targets.dshd = {
      description = "dshd instances enabled for autostart";
      wantedBy = [ "default.target" ];
    };

    environment.systemPackages = [ cfg.package ];

    environment.etc =
      lib.mapAttrs' (
        name: release: lib.nameValuePair "dshd/releases/${name}" { source = release; }
      ) releases
      // lib.optionalAttrs (defaultName != null) {
        "dshd/releases/default".source = releases.${defaultName};
      };
  };
}
