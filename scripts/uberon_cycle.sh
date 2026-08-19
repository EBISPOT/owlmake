#!/bin/bash
# Fast edit→measure loop for UBERON parity work.
#
# A full UBERON build is ~1 hour, almost all of it in two reasoning steps
# (`uberon.owl`, then `subsets/merged-partonomy.owl`). Most changes cannot
# affect those, so paying for them every cycle is waste.
#
#   uberon_cycle.sh snapshot           # save the expensive artefacts, once
#   uberon_cycle.sh affects <op>       # which artefacts' plan steps use <op>
#   uberon_cycle.sh rebuild <targets…> # restore the snapshot, rebuild only these
#   uberon_cycle.sh diff <artefacts…>  # net diff lines vs the reference
#
# The snapshot is restored with `touch` so make sees it as up to date: a
# restored file older than its prerequisites would simply be rebuilt.
set -u
OM_TREE=${OM_TREE:-/data2/ontologies/uberon-om}
REF_TREE=${REF_TREE:-/data2/ontologies/uberon-ref}
CACHE=${CACHE:-/data2/ontologies/cache-uberon}
OM_BIN=${OM_BIN:-/data2/ontologies/om-current}
ONT=$OM_TREE/src/ontology
FLAGS=(TODAY=2026-08-12 IMP=false MIR=false BRI=false PAT=false)

# The artefacts worth caching: everything whose rebuild costs reasoning.
EXPENSIVE=(
  uberon.owl uberon.obo uberon.json
  uberon-base.owl uberon-full.owl uberon-simple.owl uberon-basic.owl
  tmp/uberon.owl tmp/uberon-edit.owl
  tmp/collected-metazoan.owl tmp/collected-lifestages.owl
  tmp/collected-vertebrate.owl
  tmp/composite-metazoan.owl tmp/composite-vertebrate.owl
  collected-metazoan.owl collected-lifestages.owl
  composite-metazoan.owl composite-vertebrate.owl composite-lifestages.owl
  subsets/merged-partonomy.owl
  components/mappings.owl
)

case "${1:-}" in
snapshot)
  mkdir -p "$CACHE"
  for f in "${EXPENSIVE[@]}"; do
    [ -f "$ONT/$f" ] || continue
    mkdir -p "$CACHE/$(dirname "$f")"
    cp -p "$ONT/$f" "$CACHE/$f"
  done
  echo "snapshot: $(find "$CACHE" -type f | wc -l) files, $(du -sh "$CACHE" | cut -f1)"
  ;;
restore)
  n=0
  for f in "${EXPENSIVE[@]}"; do
    [ -f "$CACHE/$f" ] || continue
    mkdir -p "$ONT/$(dirname "$f")"
    cp -p "$CACHE/$f" "$ONT/$f" && touch "$ONT/$f" && n=$((n+1))
  done
  echo "restored $n cached artefacts (touched, so make treats them as current)"
  ;;
affects)
  # Which artefacts have a plan step using this op? Bounds a change's blast radius
  # before spending a cycle on it — `filter --axioms` turned out to be two.
  op=${2:?usage: affects <op-name>}
  python3 - "$OM_TREE/owlmake.yaml" "$op" <<'PY'
import sys, yaml
plan = yaml.safe_load(open(sys.argv[1])); op = sys.argv[2]
hits = [a['target'] for a in plan.get('artefacts', [])
        if any(s.get('op') == op for s in a.get('steps', []))]
for t in hits: print(' ', t)
print(f'{len(hits)} artefact(s) use op `{op}`')
PY
  ;;
rebuild)
  shift
  [ $# -gt 0 ] || { echo "usage: rebuild <targets…>" >&2; exit 2; }
  "$0" restore
  cd "$ONT" || exit 1
  PATH=/usr/local/bin:/usr/bin:/bin nice -n 5 "$OM_BIN" make "${FLAGS[@]}" -k "$@"
  echo "OM_RC=$?"
  ;;
diff)
  shift
  for a in "$@"; do
    r=$REF_TREE/src/ontology/$a; o=$ONT/$a
    if [ ! -f "$r" ] || [ ! -f "$o" ]; then printf '  %-40s MISSING\n' "$a"; continue; fi
    if cmp -s "$r" "$o"; then printf '  %-40s identical\n' "$a"; continue; fi
    n=$(diff "$r" "$o" 2>/dev/null | grep '^[<>]' \
        | grep -viE 'versionInfo|versionIRI|oboInOwl#date|^[<>] date:|data-version' | grep -c .)
    printf '  %-40s net=%s\n' "$a" "$n"
  done
  ;;
*)
  sed -n '2,20p' "$0"
  exit 2
  ;;
esac
