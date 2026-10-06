#!/bin/bash
# Write the Unicode block and script names owlmake's reading of
# `java.util.regex` resolves `\p{…}` against (src/dosdp/java/names.rs), and the
# cases its tests hold it to (tests/fixtures/java-regex/cases.tsv), from the
# running JDK.
#
#   scripts/gen_java_regex.sh
#
# Needs Java 21. Both outputs are committed, so the build and the tests need
# neither Java nor this script.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
java_major=$(env -u JAVA_TOOL_OPTIONS java -XshowSettings:properties -version 2>&1 \
  | sed -n 's/^ *java.specification.version = //p')
if [ "$java_major" != 21 ]; then
  echo "gen_java_regex.sh: needs Java 21, found ${java_major:-none}" >&2
  exit 1
fi
run() {
  env -u JAVA_TOOL_OPTIONS java --add-opens java.base/java.lang=ALL-UNNAMED \
    "$root/scripts/gen_java_regex.java" "$1"
}
mkdir -p "$root/src/dosdp/java" "$root/tests/fixtures/java-regex"
run tables > "$root/src/dosdp/java/names.rs"
run cases > "$root/tests/fixtures/java-regex/cases.tsv"
