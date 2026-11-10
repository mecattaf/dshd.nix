# The option set is exactly: enable, package, releases.<name>.{dsh,
# environment, unsetEnvironment}, defaultRelease, startupTimeout. Instances,
# profiles and autostart are runtime data managed with the `dshd` command,
# so none of them has an option.
{ lib, pkgs, ... }:
let
  inherit (lib) mkOption mkEnableOption types;
in
{
  options.dshd = {
    enable = mkEnableOption "dshd, durable native DSH sessions under each user's systemd user manager";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ../../pkgs/dshd.nix { };
      defaultText = lib.literalMD "the `dshd` package of this flake, built with the system's `pkgs`";
      description = ''
        The dshd package providing `dshd` and `dshd-run`. Every declared
        release pins the `dshd-run` of this package.
      '';
    };

    releases = mkOption {
      type = types.attrsOf (
        types.submodule {
          options = {
            dsh = mkOption {
              type = types.package;
              description = "The package whose `bin/dsh` this release starts.";
            };
            environment = mkOption {
              type = types.attrsOf types.str;
              default = { };
              example = {
                DSH_TELEMETRY_DISABLED = "1";
              };
              description = ''
                Variables that fill names the instance environment leaves unset.
                Merged over `DSH_PERMISSION_MODE = "danger-full-access"`, so a
                retained instance never waits for an approval nobody sees; set
                another mode here to opt out. Values land in the
                world-readable store: never secrets.
              '';
            };
            unsetEnvironment = mkOption {
              type = types.listOf types.str;
              default = [ ];
              example = [ "GITHUB_TOKEN" ];
              description = ''
                Exact variable names never recorded in an instance and never
                passed to dsh: stripped from the captured environment at
                creation and from the composed environment at every start.
              '';
            };
          };
        }
      );
      default = { };
      example = lib.literalExpression ''
        {
          stable.dsh = pkgs.callPackage ./dsh.nix { };
          candidate.dsh = pkgs.callPackage ./dsh-candidate.nix { };
        }
      '';
      description = ''
        Named releases, each built with `lib.mkRelease` and linked at
        {file}`/etc/dshd/releases/<name>`. An instance records the name it
        was created with (`dshd run --release <name>`), and `dshd update`
        follows it. Names match `^[a-z0-9]([a-z0-9_-]{0,61}[a-z0-9])?$` and
        are never shaped like an instance id (a UUID); `default` is
        reserved for the default link.
      '';
    };

    defaultRelease = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "stable";
      description = ''
        The release `dshd run` uses without `--release`, linked at
        {file}`/etc/dshd/releases/default`. `null` selects the only release
        when exactly one is declared; with several, this must name one.
      '';
    };

    startupTimeout = mkOption {
      type = types.ints.positive;
      default = 150;
      description = ''
        `TimeoutStartSec` of `dshd@.service`, in seconds: how long a start may
        take to announce readiness. A change applies at each instance's next
        start.
      '';
    };
  };
}
