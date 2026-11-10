# tests/stub/dsh.sh as a package exposing bin/dsh: the one fake DSH every
# check releases through lib.mkRelease. `version` names the store path, so
# two versions are two releases with the same behaviour.
{ pkgs, version }:
pkgs.runCommand "stub-dsh-${version}" { } ''
  install -D -m 0755 ${../tests/stub/dsh.sh} "$out/bin/dsh"
  patchShebangs "$out/bin/dsh"
''
