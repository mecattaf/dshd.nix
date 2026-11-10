# deadnix over every .nix file of the tree: no unused binding or argument.
# catches: an unused let binding, lambda argument or pattern name.
{ pkgs, ... }:
let
  inherit (pkgs) lib;
  nixFiles = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.fileFilter (file: file.hasExt "nix") ../.;
  };
in
pkgs.runCommand "dshd-deadnix" { nativeBuildInputs = [ pkgs.deadnix ]; } ''
  deadnix --fail ${nixFiles}
  touch "$out"
''
