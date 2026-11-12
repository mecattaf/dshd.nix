# One attribute per check file, fast checks first and the five NixOS VM
# checks last. crate-src.nix, stub-dsh.nix, vm-node.nix, vm-prelude.py and
# acp-client.py are shared by the checks and are not checks themselves.
{ self, pkgs }:
let
  call = file: import file { inherit self pkgs; };
in
{
  cargo-test = call ./cargo-test.nix;
  clippy = call ./clippy.nix;
  rustfmt = call ./rustfmt.nix;
  nixfmt = call ./nixfmt.nix;
  deadnix = call ./deadnix.nix;
  module = call ./module.nix;
  hygiene = call ./hygiene.nix;
  lifecycle = call ./lifecycle.nix;
  transport = call ./transport.nix;
  run-patch = call ./run-patch.nix;
  switch = call ./switch.nix;
  reboot = call ./reboot.nix;
}
