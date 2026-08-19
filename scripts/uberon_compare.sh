#!/bin/bash
# Compare every UBERON release artefact between the ODK reference tree and the
# owlmake tree.  Usage: uberon_compare.sh [ref-root] [om-root]
#
# `all_assets` writes everything into src/ontology/; only `prepare_release`
# copies to the repo root.  UBERON also COMMITS root copies of uberon-base.owl,
# uberon.obo and a whole root subsets/ directory, so the root must be the
# FALLBACK, never the preference — looking there first compares one checkout's
# committed file against the other's and calls it identical.
REF=${1:-/data2/ontologies/uberon-ref}
OM=${2:-/data2/ontologies/uberon-om}

MAIN_PRODUCTS=(
  uberon uberon-base uberon-full uberon-simple uberon-basic
  collected-metazoan collected-lifestages
  composite-metazoan composite-metazoan-basic
  composite-vertebrate composite-vertebrate-basic
  composite-lifestages common-anatomy
)
SUBSETS=(
  appendicular-minimal circulatory-minimal cranial-minimal cumbo
  digestive-minimal excretory-minimal human-view immune-minimal
  life-stages-composite life-stages-core life-stages-minimal
  merged-partonomy mouse-view musculoskeletal-minimal nephron-minimal
  nervous-minimal pulmonary-minimal renal-minimal reproductive-minimal
  sensory-minimal xenopus-view amniote-view euarchontoglires-view
)
MAPPINGS=(fbbt cl sslso biomappings uberon-local uberon import-corrections)

files=()
for n in "${MAIN_PRODUCTS[@]}";  do for f in owl obo json;     do files+=("$n.$f"); done; done
for n in "${SUBSETS[@]}";        do for f in owl obo json tsv; do files+=("subsets/$n.$f"); done; done
for n in "${MAPPINGS[@]}";       do files+=("../mappings/$n.sssom.tsv"); done
files+=(imports/merged_import.owl)
files+=(reports/uberon-edit.obo-obo-report.tsv)
files+=(../patterns/definitions.owl ../patterns/pattern.owl)

pick() { # root name -> path   (src/ontology FIRST, repo root only as fallback)
  if   [ -e "$1/src/ontology/$2" ]; then echo "$1/src/ontology/$2"
  elif [ -e "$1/$2" ];              then echo "$1/$2"
  else echo ""
  fi
}

same=0; diffs=0; missing=0
printf '%-52s %12s %12s  %s\n' FILE REF OM STATUS
for f in "${files[@]}"; do
  a=$(pick "$REF" "$f"); b=$(pick "$OM" "$f")
  printf '%-52s %12s %12s  ' "$f" \
    "$([ -n "$a" ] && stat -c%s "$a" || echo -)" \
    "$([ -n "$b" ] && stat -c%s "$b" || echo -)"
  if   [ -z "$a" ] && [ -z "$b" ]; then echo "absent-both";  missing=$((missing+1))
  elif [ -z "$a" ]; then echo "MISSING-REF"; missing=$((missing+1))
  elif [ -z "$b" ]; then echo "MISSING-OM";  missing=$((missing+1))
  elif cmp -s "$a" "$b"; then echo "identical"; same=$((same+1))
  else
    raw=$(diff "$a" "$b" 2>/dev/null | grep -c '^[<>]')
    # ignore the legitimate metadata stamps
    net=$(diff "$a" "$b" 2>/dev/null | grep '^[<>]' \
          | grep -viE 'versionInfo|versionIRI|oboInOwl#date|^[<>] date:|data-version' | grep -c .)
    echo "DIFF raw=$raw net=$net"; diffs=$((diffs+1))
  fi
done
echo
echo "identical=$same  differing=$diffs  missing=$missing"
[ "$diffs" -eq 0 ] && [ "$missing" -eq 0 ]
