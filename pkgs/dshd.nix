# The dshd crate: `bin/dshd` (lifecycle CLI) and `bin/dshd-run` (runner).
#
# The source is Cargo.toml, Cargo.lock, src/, LICENSE and NOTICE only, so
# documentation, Nix and test edits never rebuild the package. The Rust
# tests and clippy run as their own checks over the same crate plus tests/
# (checks/cargo-test.nix, checks/clippy.nix), hence doCheck = false here.
# LICENSE and NOTICE travel with the binaries, as the licence asks.
{
  lib,
  rustPlatform,
}:
let
  crate = (lib.importTOML ../Cargo.toml).package;
in
rustPlatform.buildRustPackage {
  pname = crate.name;
  inherit (crate) version;
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../LICENSE
      ../NOTICE
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  doCheck = false;
  postInstall = ''
    install -Dm0644 -t $out/share/doc/dshd LICENSE NOTICE
  '';
  meta = {
    description = "Durable native DSH sessions on NixOS";
    homepage = crate.repository;
    license = lib.licenses.asl20;
    mainProgram = "dshd";
    platforms = lib.platforms.linux;
  };
}
