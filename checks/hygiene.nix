# Public-tree hygiene: typos everywhere; no home or mount paths, deferral
# words or debugging leftovers in any file but the lock files; crates.io-only
# dependencies and no build script; a CHANGELOG whose newest entry is
# Unreleased or this version; the Apache License 2.0 with its attribution:
# an adapted file carries its SPDX identifier and its origin line together,
# and NOTICE credits the origin exactly when such a file exists.
# catches: a misspelling, a private path, an unfinished marker, a git or
# patched dependency, a version without its CHANGELOG entry, another licence
# text or SPDX identifier, an adapted file without its attribution, and a
# NOTICE that credits code the tree no longer holds or misses code it does.
{ pkgs, ... }:
let
  inherit (pkgs) lib;
  # A local checkout may hold a cargo target directory and nix result links;
  # they are build outputs, not the tree.
  resultLinks = map (name: ../. + "/${name}") (
    lib.attrNames (
      lib.filterAttrs (name: _: builtins.match "result(-.*)?" name != null) (builtins.readDir ../.)
    )
  );
  tree = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.difference ../. (
      lib.fileset.unions (
        map lib.fileset.maybeMissing (
          [
            ../doc
            ../target
          ]
          ++ resultLinks
        )
      )
    );
  };
  inherit ((lib.importTOML ../Cargo.toml).package) version;
in
pkgs.runCommand "dshd-hygiene" { nativeBuildInputs = [ pkgs.typos ]; } ''
  cd ${tree}
  typos

  # Each pattern brackets one character so this file does not match itself.
  scan() {
    if grep -rnE --exclude=Cargo.lock --exclude=flake.lock -e "$1" .; then
      echo "hygiene: $2" >&2
      exit 1
    fi
  }
  scan '/(home|mnt)[/]' "private home or mount path"
  scan '[Pp]lace[h]older|\b[f]or now\b|[f]uture work|\b([T]BD|[W]IP)\b' "deferral words"
  scan '\b([T]ODO|[F]IXME|[X]XX)\b' "deferral markers"
  scan '[t]odo!|[u]nimplemented!|[d]bg!|allow[(]dead_code' "unfinished or silenced code"

  if grep -n 'source = "git[+]' Cargo.lock || grep -n '^[[]patch' Cargo.toml; then
    echo "hygiene: dependencies come from crates.io only, unpatched" >&2
    exit 1
  fi
  if [ -e build.rs ]; then
    echo "hygiene: the crate has no build script" >&2
    exit 1
  fi
  newest=$(grep -m 1 '^## ' CHANGELOG.md || true)
  case "$newest" in
    "## Unreleased" | "## ${version} ("*) ;;
    *)
      echo "hygiene: the newest CHANGELOG.md entry is neither '## Unreleased' nor '## ${version} (date)'" >&2
      exit 1
      ;;
  esac

  if ! grep -q 'A[p]ache License' LICENSE || ! grep -q 'V[e]rsion 2[.]0, January 2004' LICENSE; then
    echo "hygiene: LICENSE is not the Apache License, Version 2.0" >&2
    exit 1
  fi
  if [ ! -f NOTICE ]; then
    echo "hygiene: NOTICE is missing" >&2
    exit 1
  fi
  spdx='S[P]DX-License-Identifier: Apache-2[.]0$'
  origin='A[d]apted from h[e]rdr-core [^ ]+ @ ce81486; changes: '
  if grep -rnE 'S[P]DX-License-Identifier:' . | grep -vE "$spdx"; then
    echo "hygiene: Apache-2.0 is the only SPDX identifier" >&2
    exit 1
  fi
  adapted=no
  for file in $(grep -rlE "$spdx|$origin" .); do
    if ! head -n 3 "$file" | grep -qE "$spdx" || ! head -n 3 "$file" | grep -qE "$origin"; then
      echo "hygiene: $file carries its SPDX identifier and its origin line together, in its first three lines" >&2
      exit 1
    fi
    adapted=yes
  done
  if grep -q '[h]erdr-core' NOTICE; then credited=yes; else credited=no; fi
  if [ "$adapted" != "$credited" ]; then
    echo "hygiene: NOTICE credits the origin exactly when a file is adapted from it (adapted: $adapted, credited: $credited)" >&2
    exit 1
  fi
  touch "$out"
''
