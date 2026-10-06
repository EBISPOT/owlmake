#!/bin/bash
# Generate the dosdp-tools fixtures for tests/dosdp.rs: for each case in
# tests/fixtures/dosdp-tools/cases.tsv the document dosdp-tools `generate`
# writes for that pattern, table and options, for each pattern (or directory)
# in prototypes.tsv the document `prototype` writes for it, for each case in
# terms.tsv the list `terms` writes, and for each case in docs.tsv the page
# `docs` writes under each release it names, once per release: 0.19.3 (shipped
# by ODK up to v1.6.1) into 0.19.3/, 0.20.0 into 0.20.0/. It also checks that
# each release refuses what refusals.tsv says it refuses.
#
#   scripts/gen_dosdp_fixtures.sh
#
# Needs Java and network access the first time (it fetches the dosdp-tools
# releases); the documents it writes are committed, so the test needs neither.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/tests/fixtures/dosdp-tools"
cd "$dir"
for version in 0.19.3 0.20.0; do
  cache="${TMPDIR:-/tmp}/owlmake-dosdp-tools-$version"
  tool="$cache/dosdp-tools-$version/bin/dosdp-tools"
  if [ ! -x "$tool" ]; then
    mkdir -p "$cache"
    curl -sfL "https://github.com/INCATools/dosdp-tools/releases/download/v$version/dosdp-tools-$version.tgz" \
      | tar -xz -C "$cache"
  fi
  mkdir -p "$version"
  grep -v '^#' cases.tsv | while IFS=$'\t' read -r expected pattern table options; do
    rm -f "$version/$expected"
    # shellcheck disable=SC2086 # the options are a list of words
    if ! env -u JAVA_TOOL_OPTIONS "$tool" generate --obo-prefixes=true \
      --template="$pattern" --infile="$table" --ontology=widgets.ofn --prefixes=prefixes.yaml \
      $options --outfile="$version/$expected" > "$cache/log" 2>&1 || [ ! -s "$version/$expected" ]; then
      cat "$cache/log" >&2
      echo "dosdp-tools $version refused $expected" >&2
      exit 1
    fi
    echo "wrote $version/$expected"
  done
  grep -v '^#' prototypes.tsv | while IFS=$'\t' read -r expected pattern; do
    rm -f "$version/$expected"
    if ! env -u JAVA_TOOL_OPTIONS "$tool" prototype --obo-prefixes=true \
      --template="$pattern" --outfile="$version/$expected" > "$cache/log" 2>&1 || [ ! -s "$version/$expected" ]; then
      cat "$cache/log" >&2
      echo "dosdp-tools $version refused prototype $expected" >&2
      exit 1
    fi
    echo "wrote $version/$expected"
  done
  grep -v '^#' terms.tsv | while IFS=$'\t' read -r expected pattern table options; do
    rm -f "$version/$expected"
    # shellcheck disable=SC2086 # the options are a list of words
    if ! env -u JAVA_TOOL_OPTIONS "$tool" terms --obo-prefixes=true \
      --template="$pattern" --infile="$table" --prefixes=prefixes.yaml \
      $options --outfile="$version/$expected" > "$cache/log" 2>&1 || [ ! -s "$version/$expected" ]; then
      cat "$cache/log" >&2
      echo "dosdp-tools $version refused terms $expected" >&2
      exit 1
    fi
    echo "wrote $version/$expected"
  done
  grep -v '^#' docs.tsv | while IFS=$'\t' read -r expected releases pattern table options; do
    [ -n "$expected" ] || continue
    case ",$releases," in *",$version,"*) ;; *) continue ;; esac
    rm -f "$version/$expected"
    # shellcheck disable=SC2086 # the options are a list of words
    if ! env -u JAVA_TOOL_OPTIONS "$tool" docs --obo-prefixes=true \
      --template="$pattern" --infile="$table" --ontology=widgets.ofn --prefixes=prefixes.yaml \
      $options --outfile="$version/$expected" > "$cache/log" 2>&1 || [ ! -s "$version/$expected" ]; then
      cat "$cache/log" >&2
      echo "dosdp-tools $version refused docs $expected" >&2
      exit 1
    fi
    echo "wrote $version/$expected"
  done
  grep -v '^#' refusals.tsv | while IFS=$'\t' read -r command releases pattern table options; do
    case ",$releases," in *",$version,"*) ;; *) continue ;; esac
    extra=""
    [ "$command" = generate ] && extra="--ontology=widgets.ofn"
    # shellcheck disable=SC2086 # the options are a list of words
    if env -u JAVA_TOOL_OPTIONS "$tool" "$command" --obo-prefixes=true \
      --template="$pattern" --infile="$table" $extra --prefixes=prefixes.yaml \
      $options --outfile="$cache/refused" > "$cache/log" 2>&1; then
      echo "dosdp-tools $version accepted $command $pattern $table $options" >&2
      exit 1
    fi
    echo "dosdp-tools $version refuses $command $pattern $table $options"
  done
done
