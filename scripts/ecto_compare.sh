#!/bin/bash
# Compare every file an ECTO build writes, between the ODK reference tree and
# the owlmake tree. Usage: compare_all.sh [ref-root] [om-root]
#
# The ODK leaves the main release files in src/ontology/ and copies them to the
# repo root only in `prepare_release`; om publishes them to the root as it goes.
# So the nine are compared ref:src/ontology/<f> against om:<f>.
REF=${1:-/home/user/ecto-ref}
OM=${2:-/home/user/ecto-om}

main=(ecto.owl ecto.obo ecto.json ecto-base.owl ecto-base.obo ecto-base.json
      ecto-full.owl ecto-full.obo ecto-full.json)
same=(
  src/ontology/imports/merged_import.owl
  src/patterns/definitions.owl src/patterns/pattern.owl
  src/ontology/reports/ecto-base.owl-obo-report.tsv
  src/ontology/reports/validate_profile_owl2dl_ecto.owl.txt
  src/ontology/reports/basic-report.tsv
  src/ontology/reports/class-count-by-prefix.tsv
  src/ontology/reports/edges.tsv
  src/ontology/reports/xrefs.tsv
  src/ontology/reports/obsoletes.tsv
  src/ontology/reports/synonyms.tsv
  src/ontology/components/bridge.owl
  src/ontology/components/ecto-xrefs.owl
  src/ontology/components/obsoletes.owl
)
while IFS= read -r f; do same+=("$f"); done < <(
  cd "$REF" && ls src/patterns/data/default/*.ofn 2>/dev/null)

report() { # name a b
  printf '%-52s %10s %10s  ' "$1" \
    "$([ -e "$2" ] && stat -c%s "$2" || echo -)" \
    "$([ -e "$3" ] && stat -c%s "$3" || echo -)"
  if   [ ! -e "$2" ]; then echo "MISSING-REF"; fail=1
  elif [ ! -e "$3" ]; then echo "MISSING-OM";  fail=1
  elif cmp -s "$2" "$3"; then echo "identical"
  else
    raw=$(diff "$2" "$3" 2>/dev/null | grep -c '^[<>]')
    srt=$(diff <(sort "$2") <(sort "$3") 2>/dev/null | grep -c '^[<>]')
    echo "DIFF raw=$raw sorted=$srt"; fail=1
  fi
}

fail=0
printf '%-52s %10s %10s  %s\n' FILE REF OM STATUS
for f in "${main[@]}"; do report "$f (release)" "$REF/src/ontology/$f" "$OM/$f"; done
for f in "${same[@]}"; do report "${f#src/}" "$REF/$f" "$OM/$f"; done
exit $fail
