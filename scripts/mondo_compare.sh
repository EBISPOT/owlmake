#!/bin/bash
# Compare every MONDO release artefact between the ODK reference tree and the
# owlmake tree.  Usage: mondo_compare.sh [ref-root] [om-root]
#
# Both builds write into src/ontology/; only `prepare_release` copies to the
# repo root.  MONDO also COMMITS root copies of subsets/* and reports/*, so the
# root must be the FALLBACK, never the preference — looking there first compares
# one checkout's committed file against the other's and calls it identical.
REF=${1:-/home/user/mondo-ref}
OM=${2:-/home/user/mondo-om}

files=(
  mondo.owl mondo.obo mondo.json
  mondo-base.owl mondo-base.obo mondo-base.json
  mondo-simple.owl mondo-simple.obo mondo-simple.json
  mondo-international.owl mondo-international.obo mondo-international.json
  mondo_nodes.tsv mondo_edges.tsv
  subsets/mondo-rare.owl subsets/mondo-rare.obo subsets/mondo-rare.json
  subsets/mondo-rare_nodes.tsv subsets/mondo-rare_edges.tsv
  subsets/mondo-clingen.owl subsets/mondo-clingen.obo subsets/mondo-clingen.json
  subsets/mondo-clingen_nodes.tsv subsets/mondo-clingen_edges.tsv
  reports/mondo_release_diff_changed_terms.tsv
  reports/mondo_release_diff_new_terms.tsv
  reports/mondo_obsoletioncandidates.tsv
  reports/source-versions.tsv
  imports/merged_import.owl
)

pick() { # root name -> path
  if   [ -e "$1/src/ontology/$2" ];   then echo "$1/src/ontology/$2"
  elif [ -e "$1/$2" ];                then echo "$1/$2"
  else echo ""
  fi
}

fail=0
printf '%-46s %11s %11s  %s\n' FILE REF OM STATUS
for f in "${files[@]}"; do
  a=$(pick "$REF" "$f"); b=$(pick "$OM" "$f")
  printf '%-46s %11s %11s  ' "$f" \
    "$([ -n "$a" ] && stat -c%s "$a" || echo -)" \
    "$([ -n "$b" ] && stat -c%s "$b" || echo -)"
  if   [ -z "$a" ]; then echo "MISSING-REF"; fail=1
  elif [ -z "$b" ]; then echo "MISSING-OM";  fail=1
  elif cmp -s "$a" "$b"; then echo "identical"
  else
    raw=$(diff "$a" "$b" 2>/dev/null | grep -c '^[<>]')
    srt=$(diff <(sort "$a") <(sort "$b") 2>/dev/null | grep -c '^[<>]')
    echo "DIFF raw=$raw sorted=$srt"; fail=1
  fi
done
exit $fail
