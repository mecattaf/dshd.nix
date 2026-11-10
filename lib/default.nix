# The flake's `lib` output: what a configuration calls to build releases
# outside the NixOS module, for example a release started with
# `dshd run --release /nix/store/...-dshd-release-... -- web`.
{ lib }:
{
  mkRelease = import ./release.nix { inherit lib; };
}
