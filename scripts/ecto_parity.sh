#!/bin/bash
# Compare every file target of an ECTO build between the ODK reference tree and
# the owlmake tree, over a surface DERIVED from `om make --list-targets`.
#
# Usage: ecto_parity.sh [ref-root] [om-root] [om-binary]
#
# The ODK leaves release artefacts in src/ontology/; om writes them to the repo
# root. A file target is compared ref:src/ontology/<t> against om:src/ontology/<t>,
# falling back to om:<root>/<t> for release artefacts.
#
# A target whose name is declared .PHONY in the Makefiles has no file of its own;
# its effects are the other files on this list, so it is skipped here and
# exercised by running it on both sides.
#
# Files older than the checkout stamp on BOTH sides were never rebuilt: a match
# there is vacuous, so they are reported as `carried`, not `identical`.
REF=${1:-/home/user/ecto-ref}
OM=${2:-/home/user/ecto-om}
OMBIN=${3:-/home/user/owlmake/target/release/om}
STAMP=${STAMP:-/home/user/ecto/.git}   # checkout instant: the clone's .git mtime

phony=$(sed -n 's/^\.PHONY:[[:space:]]*//p' "$REF/src/ontology/Makefile" "$REF/src/ontology/ecto.Makefile" | tr ' ' '\n' | sort -u)
targets=$(cd "$OM" && "$OMBIN" make --list-targets 2>/dev/null)

fail=0; identical=0; carried=0; diffn=0; missing=0; skipped=0
printf '%-70s %10s %10s  %s\n' TARGET REF OM STATUS
while IFS= read -r t; do
  case "$t" in ''|*' '*) continue;; esac
  if printf '%s\n' "$phony" | grep -qxF "$t"; then skipped=$((skipped+1)); continue; fi
  # Grouping targets with no path separator and no extension are make-level
  # names (e.g. `imports`, `subsets`, `tmp`); directories are not files either.
  r="$REF/src/ontology/$t"; o="$OM/src/ontology/$t"
  [ -d "$r" ] && { skipped=$((skipped+1)); continue; }
  # om publishes release artefacts at the repo root.
  [ -e "$o" ] || { case "$t" in */*) ;; *) o="$OM/$t";; esac; }
  [ -d "$o" ] && { skipped=$((skipped+1)); continue; }
  if [ ! -e "$r" ] && [ ! -e "$o" ]; then
    echo "ABSENT-BOTH $t"; missing=$((missing+1)); continue
  fi
  if [ ! -e "$r" ] || [ ! -e "$o" ]; then
    printf '%-70s %10s %10s  %s\n' "$t" \
      "$([ -e "$r" ] && stat -c%s "$r" || echo -)" \
      "$([ -e "$o" ] && stat -c%s "$o" || echo -)" \
      "$([ -e "$r" ] && echo MISSING-OM || echo MISSING-REF)"
    fail=1; missing=$((missing+1)); continue
  fi
  if cmp -s "$r" "$o"; then
    if [ "$r" -nt "$STAMP" ] || [ "$o" -nt "$STAMP" ]; then
      identical=$((identical+1))
    else
      carried=$((carried+1))
    fi
  else
    raw=$(diff "$r" "$o" 2>/dev/null | grep -c '^[<>]')
    srt=$(diff <(sort "$r") <(sort "$o") 2>/dev/null | grep -c '^[<>]')
    printf '%-70s %10s %10s  DIFF raw=%s sorted=%s\n' "$t" \
      "$(stat -c%s "$r")" "$(stat -c%s "$o")" "$raw" "$srt"
    fail=1; diffn=$((diffn+1))
  fi
done <<< "$targets"
echo "---"
echo "identical=$identical carried=$carried diff=$diffn missing=$missing phony/dir-skipped=$skipped"
exit $fail
