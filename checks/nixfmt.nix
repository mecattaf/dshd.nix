# nixfmt --check over every .nix file of the tree.
# catches: Nix source that is not nixfmt's layout.
{ pkgs, ... }:
let
  inherit (pkgs) lib;
  nixFiles = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.fileFilter (file: file.hasExt "nix") ../.;
  };
in
pkgs.runCommand "dshd-nixfmt" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
  find ${nixFiles} -name '*.nix' -exec nixfmt --check {} +
  touch "$out"
''
