#!/bin/bash
# Compare every file a CL build writes, between the ODK reference tree and the
# owlmake tree. Usage: cl_compare.sh [ref-root] [om-root]
#
# Both sides leave the release files in src/ontology/; `prepare_release` is what
# copies them to the repo root, and neither side has run it here.
REF=${1:-/home/user/cl-ref}
OM=${2:-/home/user/cl-om}

products=(cl cl-base cl-basic cl-full cl-non-classified cl-plus cl-simple)
main=()
for p in "${products[@]}"; do for f in owl obo json; do main+=("$p.$f"); done; done

subsets=(BDS_subset blood_and_immune_upper_slim eye_upper_slim
         general_cell_types_upper_slim kidney_upper_slim human-view mouse-view)

same=(src/ontology/imports/merged_import.owl
      src/patterns/definitions.owl src/patterns/pattern.owl
      src/ontology/reports/cl-edit.owl-obo-report.tsv
      src/ontology/reports/cl-full.owl-obo-report.tsv
      src/ontology/reports/taxon-constraint-check.txt
      src/mappings/cl-local.sssom.tsv src/mappings/cl.sssom.tsv
      src/mappings/fbbt.sssom.tsv src/mappings/zfa.sssom.tsv
      src/ontology/components/mappings.owl
      src/ontology/components/blood_and_immune_upper_slim.owl
      src/ontology/components/eye_upper_slim.owl
      src/ontology/components/general_cell_types_upper_slim.owl
      src/ontology/components/kidney_upper_slim.owl
      src/ontology/components/cellxgene_subset.owl
      src/ontology/components/PNS_neurons.owl
      src/ontology/components/2DFTU_HRA_illustrations.owl
      src/ontology/subsets/human-tags.ofn src/ontology/subsets/mouse-tags.ofn)
for s in "${subsets[@]}"; do for f in owl obo json tsv; do
  same+=("src/ontology/subsets/$s.$f"); done; done
same+=(src/patterns/data/default/ExtendedDescription.txt
       src/patterns/data/default/cellCapableOfBiologicalProcess.txt
       src/patterns/data/default/cellPartOfAnatomicalEntity.txt
       src/patterns/data/default/cyclingCellStates.txt
       src/patterns/data/clustering/cellCapableOfBiologicalProcess.txt
       src/ontology/tmp/all_pattern_terms.txt src/ontology/tmp/seed.txt
       src/ontology/tmp/simple_seed.txt
       src/ontology/reports/validate_profile_owl2dl_cl.owl.txt)
for c in stamp-component-hra_subset \
         stamp-component-blood_and_immune_upper_slim stamp-component-eye_upper_slim \
         stamp-component-general_cell_types_upper_slim stamp-component-kidney_upper_slim \
         stamp-component-cellxgene_subset stamp-component-PNS_neurons \
         stamp-component-clm-cl stamp-component-2DFTU_HRA_illustrations \
         stamp-component-wmbo-cl-comp stamp-component-bgo-cl-comp; do
  same+=("src/ontology/tmp/$c.owl"); done

# Files whose reference side the ODK does not reproduce run to run. Each is
# checked for the difference it is ALLOWED to have and reported as
# `nondeterministic-<what>`; any other difference is still a failure.
rowset=(src/ontology/reports/cl_terms.tsv src/ontology/reports/cl-edges.tsv
        src/ontology/reports/cl-synonyms.tsv src/ontology/reports/cl-xrefs.tsv
        src/ontology/reports/cl-def-xrefs.tsv src/ontology/tmp/pre_seed.txt
        src/ontology/tmp/ontologyterms.txt src/ontology/tmp/pattern_owl_seed.txt
        src/ontology/tmp/simple_seed.txt.tmp)

# `cl-plus.*` is CL merged with PCL, and it fails on both sides for the same
# reason: PCL entails an equivalence CL's reasoning policy forbids. A product
# neither side builds is reported as such, not as a missing file.

# The one line that is not owlmake's to reproduce. Where an entity carries two
# `rdfs:label`s, the label the ODK prints in an entity banner comes out of a hash
# container seeded per JVM run, so the same input gives either answer: within a
# single reference build, `human-view.owl` and `mouse-view.owl` — both written
# from the same `cl-full.owl` by the same command — disagree about
# `oboInOwl:hasDbXref`. owlmake always writes `database_cross_reference`, the
# answer the ODK gives most often. A file whose only difference is that label is
# reported as `nondeterministic-banner`, not as a failure.
banner_only() { # a b -> 0 when every differing line is a hasDbXref banner label
  local d
  d=$(diff "$1" "$2" | grep '^[<>]')
  [ -n "$d" ] || return 1
  ! grep -qv 'hasDbXref[^(]*(\(has cross-reference\|database_cross_reference\))' <<<"$d"
}

# An unordered SELECT returns its rows in the query engine's own order, and the
# reference's is not the same twice: re-running one query moved 4,290 of ~4,900
# lines. Rows carrying a blank node are excluded as well — the label the
# reference prints for one holds a UUID redrawn per run. What must match is the
# row SET.
rows_only() { # a b -> 0 when the two hold the same rows, blank nodes aside
  diff <(grep -v '_:' "$1" | sort) <(grep -v '_:' "$2" | sort) > /dev/null
}

report() { # name a b
  printf '%-56s %11s %11s  ' "$1" \
    "$([ -e "$2" ] && stat -c%s "$2" || echo -)" \
    "$([ -e "$3" ] && stat -c%s "$3" || echo -)"
  if   [ ! -e "$2" ] && [ ! -e "$3" ]; then echo "not built either side"
  elif [ ! -e "$2" ]; then echo "MISSING-REF"; fail=1
  elif [ ! -e "$3" ]; then echo "MISSING-OM";  fail=1
  elif cmp -s "$2" "$3"; then echo "identical"
  elif banner_only "$2" "$3"; then echo "nondeterministic-banner"
  else
    raw=$(diff "$2" "$3" 2>/dev/null | grep -c '^[<>]')
    srt=$(diff <(sort "$2") <(sort "$3") 2>/dev/null | grep -c '^[<>]')
    echo "DIFF raw=$raw sorted=$srt"; fail=1
  fi
}

fail=0
printf '%-56s %11s %11s  %s\n' FILE REF OM STATUS
for f in "${main[@]}"; do report "$f" "$REF/src/ontology/$f" "$OM/src/ontology/$f"; done
for f in "${same[@]}"; do report "$f" "$REF/$f" "$OM/$f"; done
for f in "${rowset[@]}"; do
  a=$REF/$f; b=$OM/$f
  printf '%-56s %11s %11s  ' "$f" \
    "$([ -e "$a" ] && stat -c%s "$a" || echo -)" "$([ -e "$b" ] && stat -c%s "$b" || echo -)"
  if   [ ! -e "$a" ]; then echo "MISSING-REF"; fail=1
  elif [ ! -e "$b" ]; then echo "MISSING-OM";  fail=1
  elif cmp -s "$a" "$b"; then echo "identical"
  elif rows_only "$a" "$b"; then echo "nondeterministic-row-order"
  else echo "DIFF rows=$(diff <(sort "$a") <(sort "$b") | grep -c '^[<>]')"; fail=1
  fi
done
# Build intermediates, reported but not gated: an `.ofn` the plan names carries
# two leading `#` comment lines holding the prefix context functional syntax
# cannot express, which the next step reads back, and `tmp/merged-cl-edit.ofn`
# is missing one banner label — the reference resolves banner labels across
# every ontology its manager has loaded, including the import closure it has
# just removed. Neither reaches a release artefact.
echo
echo 'build intermediates (reported, not gated):'
for f in src/ontology/tmp/cl-preprocess.owl src/ontology/tmp/merged-cl-edit.ofn \
         src/ontology/tmp/validate.ofn src/ontology/tmp/cl-plus-taxon-disjoints.ofn; do
  a=$REF/$f; b=$OM/$f
  printf '%-56s %11s %11s  ' "$f" \
    "$([ -e "$a" ] && stat -c%s "$a" || echo -)" "$([ -e "$b" ] && stat -c%s "$b" || echo -)"
  if [ ! -e "$a" ] || [ ! -e "$b" ]; then echo "not built"
  elif cmp -s "$a" "$b"; then echo "identical"
  else echo "differs by $(diff "$a" "$b" | grep -c '^[<>]') line(s)"
  fi
done

exit $fail
