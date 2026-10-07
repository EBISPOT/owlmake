#!/bin/bash
# Write the cases owlmake's literals are held to
# (tests/fixtures/owlapi-literals/cases.tsv): lexical forms, the datatype or
# language tag each is made with, and what OWL API's data factory makes of them.
#
#   scripts/gen_owlapi_literals.sh path/to/robot.jar
#
# Needs Java 21 and ROBOT 1.9.11's jar, which carries OWL API 4.5.29. The output
# is committed, so the build and the tests need neither.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
jar=${1:?usage: gen_owlapi_literals.sh path/to/robot.jar}
java_major=$(env -u JAVA_TOOL_OPTIONS java -XshowSettings:properties -version 2>&1 \
  | sed -n 's/^ *java.specification.version = //p')
if [ "$java_major" != 21 ]; then
  echo "gen_owlapi_literals.sh: needs Java 21, found ${java_major:-none}" >&2
  exit 1
fi
owlapi=$(unzip -p "$jar" META-INF/maven/net.sourceforge.owlapi/owlapi-api/pom.properties | sed -n 's/^version=//p')
mkdir -p "$root/tests/fixtures/owlapi-literals"
env -u JAVA_TOOL_OPTIONS java -cp "$jar" "$root/scripts/gen_owlapi_literals.java" "$owlapi" \
  > "$root/tests/fixtures/owlapi-literals/cases.tsv"
