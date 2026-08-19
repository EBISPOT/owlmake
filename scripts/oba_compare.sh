#!/bin/bash
# Compare every OBA release artefact between the ODK reference tree and the
# owlmake tree.  Usage: oba_compare.sh [ref-root] [om-root] [stamp-file]
#
# om publishes the twelve main files to the REPO ROOT as it goes; the ODK leaves
# them in src/ontology/ because `prepare_release` is what copies them up.  So a
# name is looked up in src/ontology/ first and at the root second — and never the
# other way round, because OBA COMMITS root copies of oba-base.owl and oba.obo,
# and preferring those compares one checkout against another and calls it
# identical.
#
# The same trap in time: pass a stamp file created immediately before the om
# build and any om output older than it is reported STALE rather than identical.
#
# imports/merged_import.owl is listed even for an IMP=false run, where neither
# side rebuilds it.  Dropping it there would report 15 of 16 and leave the
# sixteenth unexamined; it is compared and labelled `identical (pinned)`.
REF=${1:-/home/user/oba-ref}
OM=${2:-/home/user/oba-om}
STAMP=${3:-}

files=(
  oba.owl oba.obo oba.json
  oba-base.owl oba-base.obo oba-base.json
  oba-basic.owl oba-basic.obo oba-basic.json
  oba-full.owl oba-full.obo oba-full.json
  ../patterns/definitions.owl ../patterns/pattern.owl
  reports/oba.owl-obo-report.tsv
  imports/merged_import.owl
)

pick() { # root name -> path
  local p
  p=$(realpath -m "$1/src/ontology/$2")
  if   [ -e "$p" ];    then echo "$p"
  elif [ -e "$1/$2" ]; then echo "$1/$2"
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
  elif cmp -s "$a" "$b"; then
    if [ -n "$STAMP" ] && [ ! "$b" -nt "$STAMP" ]; then echo "identical (pinned)"
    else echo "identical"; fi
  elif [ -n "$STAMP" ] && [ ! "$b" -nt "$STAMP" ]; then echo "STALE-OM"; fail=1
  else
    raw=$(diff "$a" "$b" 2>/dev/null | grep -c '^[<>]')
    srt=$(diff <(sort "$a") <(sort "$b") 2>/dev/null | grep -c '^[<>]')
    echo "DIFF raw=$raw sorted=$srt"; fail=1
  fi
done
exit $fail
