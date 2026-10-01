#!/bin/bash
# Generate an oracle fixture for `update_repo` (tests/update_repo.rs): what
# ODK's own `odk.py update` does to a repository.
#
#   scripts/gen_odk_update_fixture.sh <name> <repository>
#
# <repository> is a directory laid out from the repository root, holding at
# least src/ontology/<id>-odk.yaml and the edit file. Writes
# tests/fixtures/odk-<version>-update/<name>/before (the repository, its
# configuration as an owlmake.yaml) and .../after (the files owlmake's
# update_repo is answerable for, as ODK left them).
#
# Needs `uv`, network access the first time, and `robot` on the PATH with the
# ODK plugin in $ROBOT_PLUGINS_DIRECTORY: ODK edits the import declarations by
# running `robot odk:import`. Every import the edit file declares must resolve
# (through the catalog, to a file in <repository>), or that command fails.
set -euo pipefail
VERSION=1.6.1
name=$1; repository=$(cd "$2" && pwd)
root=$(cd "$(dirname "$0")/.." && pwd)
cache="${TMPDIR:-/tmp}/owlmake-odk-$VERSION-full"
[ -d "$cache/template" ] || { mkdir -p "$cache" && curl -sfL \
  "https://github.com/INCATools/ontology-development-kit/archive/refs/tags/v$VERSION.tar.gz" \
  | tar -xz --strip-components=1 -C "$cache"; }
out="$root/tests/fixtures/odk-$VERSION-update/$name"
work=$(mktemp -d)
rm -rf "$out"; mkdir -p "$out/before" "$out/after"
cp -R "$repository/." "$work/"
(cd "$work/src/ontology" && ODK_VERSION="v$VERSION" uv run --quiet \
    --with click --with jinja2 --with dacite --with pyyaml --with dataclasses-json \
    --with dataclasses-jsonschema --with defusedxml \
    python "$cache/odk/odk.py" update -T "$cache/template/" >/dev/null 2>&1) \
  || { echo "odk.py update failed in $work" >&2; exit 1; }
cp -R "$repository/src" "$out/before/"
config=$(ls "$out"/before/src/ontology/*-odk.yaml)
{ echo "emulate_odk_version: $VERSION"; cat "$config"; } > "$out/before/owlmake.yaml"
rm "$config"
# What is about ODK itself is not update_repo's: see src/odk/update.rs.
(cd "$work" && find src -type f \
    | grep -v -e 'README' -e 'Makefile$' -e '/run\.' -e 'run-command' -e '^src/metadata/' \
              -e 'tmp/.gitkeep' -e '-odk\.yaml$' -e '/example\.' \
    | while read -r f; do mkdir -p "$out/after/$(dirname "$f")"; cp "$f" "$out/after/$f"; done)
rm -rf "$work"
echo "wrote $out ($(find "$out/after" -type f | wc -l | tr -d ' ') files after)"
