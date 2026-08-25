#!/bin/bash
# Replay every OBA mirror recipe with `om` over the ODK's OWN saved download, and
# compare against the mirror the ODK wrote.
#
# Usage: oba_mirror_replay.sh [ref-repo] [om-binary] [out-dir] [version]
#
# The mirror set the release comparison uses is built once by the ODK and
# hard-linked into both trees, so `om`'s own mirror recipes would otherwise go
# untested: two downloads of the same PURL minutes apart are not a comparison of
# two implementations.  Replaying each recipe over `tmp/<id>-download.owl` — which
# the recipe keeps — compares the two implementations over identical bytes.
#
# A recipe that reads `--input-iri` has no saved download, so its replay re-fetches
# and is marked `refetch`: identical bytes there rest on the PURL not moving
# between the two runs.
set -u
REF=${1:-/data2/ontologies/oba-ref}/src/ontology
OMBIN=${2:-/data2/owlmake-oba/target/release/om}
OUT=${3:-./mirror-replay}
VERSION=${4:-$(date +%Y-%m-%d)}
OM="$OMBIN --catalog $REF/catalog-v001.xml"
mkdir -p "$OUT"; OUT=$(realpath "$OUT"); cd "$REF" || exit 1
U=http://purl.obolibrary.org/obo
run() { # id  note  args...
  local id=$1 note=$2; shift 2
  $OM "$@" -o "$OUT/$id.owl" >"$OUT/$id.log" 2>&1
  local rc=$?
  if [ $rc -ne 0 ]; then printf '%-14s %-10s ERROR (see %s.log)\n' "$id" "$note" "$id"; return; fi
  if cmp -s "$OUT/$id.owl" "$REF/mirror/$id.owl"; then printf '%-14s %-10s IDENTICAL\n' "$id" "$note"
  else printf '%-14s %-10s DIFF %s lines\n' "$id" "$note" "$(diff "$REF/mirror/$id.owl" "$OUT/$id.owl" 2>/dev/null | grep -c '^[<>]')"; fi
}
EXT="--axioms external --preserve-structure false --trim false"
run ro        saved   convert -i tmp/ro-download.owl
run pato      saved   convert -i tmp/pato-download.owl
run omo       saved   convert -i tmp/omo-download.owl
run hp        saved   convert -i tmp/hp-download.owl
run mondo     saved   convert -i tmp/mondo-download.owl
run nbo       saved   convert -i tmp/nbo-download.owl
run go        saved   remove -i tmp/go-download.owl --base-iri $U/GOCHE_ --base-iri $U/GO_ --base-iri $U/GOREL_ $EXT
run uberon    saved   remove -i tmp/uberon-download.owl --base-iri $U/UBERON $EXT
run cl        saved   remove -i tmp/cl-download.owl --base-iri $U/CL $EXT
run so        saved   remove -i tmp/so-download.owl --base-iri $U/SO $EXT
run po        saved   remove -i tmp/po-download.owl --base-iri $U/PO $EXT
run bfo       saved   remove -i tmp/bfo-download.owl --base-iri $U/BFO $EXT
run obi       saved   remove -i tmp/obi-download.owl --base-iri $U/OBI $EXT
run goplus    refetch convert -I $U/go/go-base.owl
run ncbitaxon refetch convert -I $U/ncbitaxon/subsets/taxslim.owl
run chebi     refetch remove -I $U/upheno/chebi_slim.owl --base-iri $U/CHEBI $EXT
run pr        refetch remove -I https://raw.githubusercontent.com/obophenotype/pro_obo_slim/master/pr_slim.owl --base-iri $U/PR $EXT
run ncit      refetch remove -I https://raw.githubusercontent.com/ncit-obo-org/ncit-obo-edition/refs/heads/main/src/ontology/ncit-obo-slim.owl --base-iri $U/NCIT $EXT
run swisslipids saved merge -i tmp/sl_subclassof.ttl -i tmp/sl_metadata.ttl -i tmp/sl_partof.ttl -i tmp/sl_haspart.ttl -i tmp/sl_subclasslipid.ttl reason reduce convert
$OM template --prefix "LM: https://bioregistry.io/lipidmaps:" --template ../templates/lipidmaps.tsv \
  annotate --ontology-iri $U/oba/mirror/lipidmaps.owl \
  annotate -V $U/oba/releases/$VERSION/mirror/lipidmaps.owl --annotation owl:versionInfo $VERSION \
  convert -f ofn --output "$OUT/lipidmaps.owl" >"$OUT/lipidmaps.log" 2>&1
if cmp -s "$OUT/lipidmaps.owl" "$REF/mirror/lipidmaps.owl"; then printf '%-14s %-10s IDENTICAL\n' lipidmaps template
else printf '%-14s %-10s DIFF %s lines\n' lipidmaps template "$(diff "$REF/mirror/lipidmaps.owl" "$OUT/lipidmaps.owl" 2>/dev/null | grep -c '^[<>]')"; fi

# mirror/merged.owl: the ODK's own merge rule over the twenty mirrors, in $(IMPORTS) order.
$OM merge $(for m in ro chebi goplus go pato omo hp mondo ncbitaxon uberon cl nbo pr so po bfo swisslipids lipidmaps ncit obi; do printf ' -i mirror/%s.owl' $m; done) -o "$OUT/merged.owl" >"$OUT/merged.log" 2>&1
if cmp -s "$OUT/merged.owl" "$REF/mirror/merged.owl"; then printf '%-14s %-10s IDENTICAL\n' merged merge
else printf '%-14s %-10s DIFF %s lines\n' merged merge "$(diff "$REF/mirror/merged.owl" "$OUT/merged.owl" 2>/dev/null | grep -c '^[<>]')"; fi
