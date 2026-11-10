# mkRelease { pkgs, dsh, dshd ? <this flake's dshd for pkgs>, environment ? { }, unsetEnvironment ? [ ] }
#
# Builds "dshd-release-<dsh name>", the one store path an instance's
# `current` and `booted` links point at:
#
#   bin/dsh                 symlink to <dsh>/bin/dsh
#   bin/dshd-run            symlink to <dshd>/bin/dshd-run
#   share/dshd/release.json {"environment": {...}, "unsetEnvironment": [...]}
#
# `environment` is merged over { DSH_PERMISSION_MODE = "danger-full-access"; }
# and fills only names the instance environment leaves unset. Values land in
# the world-readable store, so they are never secrets. `unsetEnvironment`
# names variables stripped at creation and at every start.
{ lib }:
{
  pkgs,
  dsh,
  dshd ? pkgs.callPackage ../pkgs/dshd.nix { },
  environment ? { },
  unsetEnvironment ? [ ],
}:
let
  inherit (builtins) toJSON;

  merged = {
    DSH_PERMISSION_MODE = "danger-full-access";
  }
  // environment;

  # The runner sets these itself after the strip, so listing them would
  # promise a strip that never happens.
  runnerSet = [
    "DSH_HOME"
    "PWD"
    "INVOCATION_ID"
    "JOURNAL_STREAM"
  ];

  malformed = lib.filter (name: name == "" || lib.hasInfix "=" name) unsetEnvironment;
  overlapping = lib.filter (name: merged ? ${name}) unsetEnvironment;
  reserved = lib.intersectLists runnerSet unsetEnvironment;

  problems =
    lib.optional (malformed != [ ]) "names must be non-empty and contain no '=': ${toJSON malformed}"
    ++ lib.optional (overlapping != [ ]) "names also set by environment: ${toJSON overlapping}"
    ++ lib.optional (reserved != [ ]) "names the runner always sets: ${toJSON reserved}";

  releaseJson = toJSON {
    environment = merged;
    inherit unsetEnvironment;
  };
in
assert lib.assertMsg (problems == [ ]) (
  "lib.mkRelease: invalid unsetEnvironment, " + lib.concatStringsSep "; " problems
);
pkgs.runCommand "dshd-release-${dsh.name}" { } ''
  mkdir -p "$out/bin" "$out/share/dshd"
  ln -s ${lib.getExe' dsh "dsh"} "$out/bin/dsh"
  ln -s ${lib.getExe' dshd "dshd-run"} "$out/bin/dshd-run"
  test -x "$out/bin/dsh" || {
    echo "lib.mkRelease: ${dsh} has no executable bin/dsh" >&2
    exit 1
  }
  test -x "$out/bin/dshd-run" || {
    echo "lib.mkRelease: ${dshd} has no executable bin/dshd-run" >&2
    exit 1
  }
  printf '%s\n' ${lib.escapeShellArg releaseJson} > "$out/share/dshd/release.json"
''
