#!/bin/bash
# Compare uPheno's release artefacts between an ODK reference tree and an om tree.
#
#   upheno_compare.sh <ref-tree> <om-tree>
#
# Both arguments are repository ROOTS (the directory holding `src/ontology`).
# Every artefact `prepare_release_customised` rsyncs into the release directory is
# compared where it is BUILT — `src/ontology` — plus the two pattern products and
# the refreshed-import files, which the release consumes but does not itself copy.
set -u

ref=${1:?usage: upheno_compare.sh <ref-tree> <om-tree>}
om=${2:?usage: upheno_compare.sh <ref-tree> <om-tree>}

PRODUCTS="upheno upheno-base upheno-basic upheno-full upheno-base-with-bridge
          upheno-equivalence-model upheno-old-model upheno-curated upheno-curated-with-sspo"
FORMATS="owl obo json"

# RELEASE_ASSETS = MAIN_FILES + SUBSET_FILES (empty for uPheno) + REPORT_FILES,
# and prepare_release_customised additionally copies PATTERN_RELEASE_FILES.
files=()
for p in $PRODUCTS; do
  for f in $FORMATS; do files+=("src/ontology/$p.$f"); done
done
files+=("src/ontology/reports/upheno-base.owl-obo-report.tsv")
files+=("src/patterns/definitions.owl" "src/patterns/pattern.owl")

# Not release assets, but what a refreshed-imports run produces and every
# artefact above is built from.
imports=("src/ontology/imports/merged_import.owl" "src/ontology/mirror/merged.owl")

same=0; diff=0; missing=0; absent=0
printf '%-58s %s\n' "ARTEFACT" "RESULT"
printf '%s\n' "----------------------------------------------------------------------"

# A reference artefact may be held gzipped (the two release trees together do not
# fit on disk uncompressed); compare against the decompressed stream when so.
cat_ref() { if [ -f "$1" ]; then cat "$1"; else gzip -dc "$1.gz"; fi; }
has_ref() { [ -f "$1" ] || [ -f "$1.gz" ]; }

compare() {
  local rel=$1 a="$ref/$1" b="$om/$1"
  if ! has_ref "$a" && [ ! -f "$b" ]; then
    printf '%-58s %s\n' "$rel" "absent from BOTH"; absent=$((absent+1)); return
  fi
  if ! has_ref "$a"; then
    printf '%-58s %s\n' "$rel" "MISSING in ref"; missing=$((missing+1)); return
  fi
  if [ ! -f "$b" ]; then
    printf '%-58s %s\n' "$rel" "MISSING in om"; missing=$((missing+1)); return
  fi
  if cat_ref "$a" | cmp -s - "$b"; then
    printf '%-58s %s\n' "$rel" "identical"; same=$((same+1))
  else
    local n
    n=$(diff <(cat_ref "$a") <(cat "$b") 2>/dev/null | grep -c '^[<>]')
    printf '%-58s %s\n' "$rel" "DIFFERS ($n lines)"; diff=$((diff+1))
  fi
}

for rel in "${files[@]}"; do compare "$rel"; done
printf '%s\n' "-- refreshed imports --"
for rel in "${imports[@]}"; do compare "$rel"; done

# `absent from BOTH` is counted apart from `missing`: an artefact neither build can
# produce (uPheno's old model needs a `metazoa.owl` release asset that upstream
# 404s) is not a parity failure, but it is not a pass either, so it is reported
# rather than folded into `identical`.
printf '\nidentical=%d differs=%d missing=%d absent-from-both=%d\n' \
  "$same" "$diff" "$missing" "$absent"
[ "$diff" -eq 0 ] && [ "$missing" -eq 0 ]
