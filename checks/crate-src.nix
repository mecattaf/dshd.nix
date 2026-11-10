# The crate with its tests: the package source plus tests/, clippy.toml and
# the README whose command examples the CLI integration tests parse.
# cargo-test, clippy and rustfmt read it; the package never does, so a test
# edit rebuilds neither the package nor the VM checks.
{ lib }:
lib.fileset.toSource {
  root = ../.;
  fileset = lib.fileset.unions [
    ../Cargo.toml
    ../Cargo.lock
    ../clippy.toml
    ../src
    ../tests
    ../LICENSE
    ../NOTICE
    ../README.md
  ];
}
