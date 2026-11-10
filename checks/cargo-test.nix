# cargo test of the crate: unit tests and tests/*.rs, with tests/stub first
# on PATH for the integration tests that spawn the stub dsh and the fake
# tools. DSHD_GOLDEN_RELEASE names a release of the stub dsh, so tests/cli.rs
# runs the binary against every golden case and tests/contract.rs checks the
# JSON they print against the machine contract. It reuses the package
# recipe, so the release profile's dependency build is shared with the
# build phase.
# catches: a unit or integration test turned red by any change to the crate,
# a changed verb output, outcome, error kind, exit code or tool argv in a
# golden case, a README example that no longer parses, and a contract break.
{ self, pkgs }:
let
  goldenRelease = self.lib.mkRelease {
    inherit pkgs;
    unsetEnvironment = [ "PLANTED_SCRUBBED" ];
    dsh = import ./stub-dsh.nix {
      inherit pkgs;
      version = "golden";
    };
  };
in
self.packages.${pkgs.stdenv.hostPlatform.system}.dshd.overrideAttrs (previous: {
  pname = "dshd-cargo-test";
  src = import ./crate-src.nix { inherit (pkgs) lib; };
  doCheck = true;
  cargoTestFlags = [ "--locked" ];
  # The stub and the fake tools are POSIX sh scripts.
  nativeCheckInputs = [ pkgs.bash ];
  env = previous.env or { } // {
    DSHD_GOLDEN_RELEASE = "${goldenRelease}";
  };
  preCheck = ''
    export PATH="$PWD/tests/stub:$PATH"
  '';
})
