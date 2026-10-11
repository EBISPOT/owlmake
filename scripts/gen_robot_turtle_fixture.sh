#!/bin/bash
# Generate a reference fixture for a Turtle or N-Triples input: what ROBOT writes
# for it, with the blank-node names fixed to the ones owlmake gives.
#
#   scripts/gen_robot_turtle_fixture.sh <robot.jar> <input.{ttl,nt}> <output.{ofn,owl,ttl,…}>
#
# ROBOT reads Turtle and N-Triples with RDF4J, which names an unlabelled node
# node<clock in base 32>x<n> and a labelled one genid-<random UUID>-<label>, and
# OWL API files the statements in hash tables keyed on those names: the order
# it translates them in, and so the numbers its anonymous individuals take,
# change from run to run. This runs ROBOT's own load and save (IOHelper) with
# the clock part fixed to 1hf7uaq00 and the UUID to
# 0123456789ab4cde8f0123456789abcd, which makes the output one ROBOT run's
# output, every time.
#
# Needs a Java 11 `jrunscript` (Nashorn) on PATH, or JRUNSCRIPT set to one.
set -euo pipefail
jar=$1; input=$2; output=$3
script=$(mktemp --suffix=.js)
trap 'rm -f "$script"' EXIT
cat > "$script" <<'EOF'
var File = Java.type("java.io.File");
var SimpleValueFactory = Java.type("org.eclipse.rdf4j.model.impl.SimpleValueFactory");
var TurtleParser = Java.type("org.eclipse.rdf4j.rio.turtle.TurtleParser");
var NTriplesParser = Java.type("org.eclipse.rdf4j.rio.ntriples.NTriplesParser");
var AbstractRDFParser = Java.type("org.eclipse.rdf4j.rio.helpers.AbstractRDFParser");
var RDFFormat = Java.type("org.eclipse.rdf4j.rio.RDFFormat");
var RDFParserFactory = Java.type("org.eclipse.rdf4j.rio.RDFParserFactory");
var RDFParserRegistry = Java.type("org.eclipse.rdf4j.rio.RDFParserRegistry");
var IOHelper = Java.type("org.obolibrary.robot.IOHelper");
var vf = SimpleValueFactory.getInstance();
var prefix = vf.getClass().getDeclaredField("bnodePrefix");
prefix.setAccessible(true);
prefix.set(vf, "node1hf7uaq00x");
var next = vf.getClass().getDeclaredField("nextBNodeID");
next.setAccessible(true);
next.setInt(vf, 1);
var uuid = AbstractRDFParser.class.getDeclaredField("nextBNodePrefix");
uuid.setAccessible(true);
function pin(Parser, format) {
  var Pinned = Java.extend(Parser);
  RDFParserRegistry.getInstance().add(new RDFParserFactory({
    getRDFFormat: function() { return format; },
    getParser: function() {
      var sup;
      var p = new Pinned({
        clear: function() {
          sup.clear();
          uuid.set(p, "0123456789ab4cde8f0123456789abcd-");
        }
      });
      sup = Java.super(p);
      return p;
    }
  }));
}
pin(TurtleParser, RDFFormat.TURTLE);
pin(NTriplesParser, RDFFormat.NTRIPLES);
var io = new IOHelper();
io.saveOntology(io.loadOntology(new File(arguments[0])), new File(arguments[1]));
EOF
"${JRUNSCRIPT:-jrunscript}" -cp "$jar" "$script" "$input" "$output"
