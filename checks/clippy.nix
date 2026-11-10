# cargo clippy over every target, tests included, with the Cargo.toml lints
# and clippy.toml; any warning fails the check.
# catches: a lint the crate denies, in product or test code.
{ self, pkgs }:
self.packages.${pkgs.stdenv.hostPlatform.system}.dshd.overrideAttrs (previous: {
  pname = "dshd-clippy";
  src = import ./crate-src.nix { inherit (pkgs) lib; };
  nativeBuildInputs = previous.nativeBuildInputs ++ [ pkgs.clippy ];
  buildPhase = ''
    cargo clippy --offline --locked --all-targets -- -D warnings
  '';
  installPhase = ''
    touch "$out"
  '';
})
