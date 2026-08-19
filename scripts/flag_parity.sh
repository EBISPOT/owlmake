#!/usr/bin/env bash
# Flag-parity check: for each command, compare the set of ROBOT long options
# against owlmake's. Reports any ROBOT option owlmake is missing (and extras).
# Usage: scripts/flag_parity.sh [path-to-owlmake-binary]
# Requires `robot` on PATH.
set -u

OWLMAKE="${1:-./target/debug/om}"

# ROBOT command -> owlmake subcommand name (kebab). Only commands ROBOT has.
declare -A MAP=(
  [convert]=convert [merge]=merge [unmerge]=unmerge [reason]=reason
  [reduce]=reduce [relax]=relax [materialize]=materialize [diff]=diff
  [annotate]=annotate [filter]=filter [remove]=remove [extract]=extract
  [measure]=measure [query]=query [verify]=verify [report]=report
  [template]=template [rename]=rename [export]=export
  [export-prefixes]=export-prefixes [repair]=repair [expand]=expand
  [validate-profile]=validate-profile [explain]=explain
  [mirror]=mirror
)

# Options owlmake provides globally (so a per-command miss is not a real gap).
GLOBAL="--input --input-iri --input-format --output --format --prefixes --prefix --add-prefix --add-prefixes --noprefixes --xml-entities --catalog --strict --verbose --very-verbose --very-very-verbose --help --version"

longs() { grep -oE -- '--[a-z][a-z0-9-]+' | sort -u; }

total_missing=0
for rcmd in "${!MAP[@]}"; do
  ocmd="${MAP[$rcmd]}"
  rhelp=$(robot "$rcmd" --help 2>/dev/null | longs)
  ohelp=$("$OWLMAKE" "$ocmd" --help 2>/dev/null | longs)
  [ -z "$rhelp" ] && { echo "skip $rcmd (no robot help)"; continue; }
  present=$(printf '%s\n' $ohelp $GLOBAL | sort -u)
  miss=""
  for opt in $rhelp; do
    printf '%s\n' "$present" | grep -qxF -- "$opt" || miss="$miss $opt"
  done
  if [ -n "$miss" ]; then
    echo "MISSING in owlmake $ocmd:$miss"
    total_missing=$((total_missing + $(echo $miss | wc -w)))
  else
    echo "OK $ocmd"
  fi
done
echo "---"
echo "total missing options: $total_missing"
