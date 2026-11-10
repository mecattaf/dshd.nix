# cargo fmt --check over the crate and its tests; compiles nothing.
# catches: Rust source that is not rustfmt's layout.
{ pkgs, ... }:
pkgs.runCommand "dshd-rustfmt"
  {
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.rustfmt
    ];
  }
  ''
    cp -r --no-preserve=mode ${import ./crate-src.nix { inherit (pkgs) lib; }} crate
    cd crate
    export CARGO_HOME="$TMPDIR/cargo"
    cargo fmt --check
    touch "$out"
  ''
