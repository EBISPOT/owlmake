#!/bin/bash
# Generate an oracle fixture for the built-in rules (tests/builtin_rules.rs):
# the Makefile that ODK itself generates for a configuration.
#
#   scripts/gen_odk_fixture.sh <name> <config.yaml> [<own-rules.Makefile>]
#
# writes tests/fixtures/odk-<version>/<name>/{config.yaml,Makefile[,own.Makefile]}.
# Needs `uv` and network access the first time (it fetches ODK's generator and
# template at the pinned tag); the fixtures it writes are committed, so the test
# itself needs neither.
set -euo pipefail
VERSION=1.6.1
name=$1; config=$2; own=${3:-}
root=$(cd "$(dirname "$0")/.." && pwd)
cache="${TMPDIR:-/tmp}/owlmake-odk-$VERSION"
base="https://raw.githubusercontent.com/INCATools/ontology-development-kit/v$VERSION"
mkdir -p "$cache/template/src/ontology"
[ -s "$cache/odk.py" ] || curl -sfL "$base/odk/odk.py" -o "$cache/odk.py"
[ -s "$cache/template/src/ontology/Makefile.jinja2" ] || \
  curl -sfL "$base/template/src/ontology/Makefile.jinja2" -o "$cache/template/src/ontology/Makefile.jinja2"
out="$root/tests/fixtures/odk-$VERSION/$name"
mkdir -p "$out"
cp "$config" "$out/config.yaml"
[ -n "$own" ] && cp "$own" "$out/own.Makefile"
(cd "$cache" && ODK_VERSION="v$VERSION" uv run --quiet \
    --with click --with jinja2 --with dacite --with pyyaml --with dataclasses-json \
    --with dataclasses-jsonschema --with defusedxml \
    python odk.py create-makefile -C "$out/config.yaml" -T template \
    -i template/src/ontology/Makefile.jinja2) > "$out/Makefile"
echo "wrote $out ($(wc -l < "$out/Makefile") lines)"
