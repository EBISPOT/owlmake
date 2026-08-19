// Node smoke test for the owlmake wasm package. Run after `wasm-pack build
// --target nodejs --out-dir pkg`:  node test/smoke.mjs
import { Ontology, MappingSet, sssomConvert, dosdp } from "../pkg/owlmake_wasm.js";
import assert from "node:assert";

const enc = new TextEncoder();
const dec = new TextDecoder();

const ONT = `Prefix(:=<http://x.org/>)
Ontology(<http://x.org/o>
Declaration(Class(:A))
Declaration(Class(:B))
Declaration(Class(:C))
EquivalentClasses(:A :B)
SubClassOf(:B :C)
)
`;

// parse + read accessors
const ont = Ontology.parse(enc.encode(ONT), "ofn");
assert.equal(ont.axiomCount(), 6);
assert.deepEqual(ont.classes(), [
  "http://x.org/A",
  "http://x.org/B",
  "http://x.org/C",
]);

// edit via functional-syntax fragment
assert.equal(ont.addAxioms("Declaration(Class(:D))\nSubClassOf(:D :A)"), 2);
assert.ok(ont.classes().includes("http://x.org/D"));
assert.equal(ont.addAxioms("SubClassOf(:D :A)"), 0); // set semantics: no-op

// reason (built-in EL)
ont.reason("elk");
const pairs = ont.subclassPairs();
assert.ok(pairs.some(([s, o]) => s === "http://x.org/D" && o === "http://x.org/A"));

// serialize round-trips
const out = dec.decode(ont.serialize("ofn"));
assert.ok(out.includes("SubClassOf"));

// remove
assert.equal(ont.removeAxioms("SubClassOf(:D :A)"), 1);

// merge
const other = Ontology.parse(
  enc.encode("Prefix(:=<http://y.org/>)\nOntology(<http://y.org/o>\nDeclaration(Class(:Z))\n)\n"),
  "ofn",
);
ont.merge(other);
assert.ok(ont.classes().includes("http://y.org/Z"));

// --- in-memory command methods ---
const cmd = Ontology.parse(enc.encode(ONT), "ofn");
assert.ok(cmd.measure().includes("classes"));
// `query` returns SPARQL TSV, in which a literal keeps its full Turtle form —
// `"8"^^<http://www.w3.org/2001/XMLSchema#integer>` — so the count has to be
// lifted out of the quotes rather than parsed as a bare number.
const countRow = cmd.query("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }").trim().split("\n").pop();
assert.ok(Number(countRow.replace(/^"(.*?)"\^\^.*$/, "$1")) > 0, `unexpected count row: ${countRow}`);
const mod = cmd.extract(["http://x.org/A", "http://x.org/B"], "STAR");
assert.ok(mod.classes().includes("http://x.org/A"));
cmd.annotate("http://x.org/o2", null, ["http://www.w3.org/2000/01/rdf-schema#comment", "hi"]);
cmd.remove(["http://x.org/C"], []);
assert.ok(!cmd.classes().includes("http://x.org/C"));
cmd.materialize([]);
assert.equal(
  Ontology.parse(enc.encode(ONT), "ofn").diff(Ontology.parse(enc.encode(ONT), "ofn")).trim(),
  "Ontologies are identical (no logical differences).",
);
// rename round-trips through RDF/XML (horned-owl reads and writes it on wasm too).
cmd.rename({ "http://x.org/A": "http://x.org/AA" });
assert.ok(cmd.classes().includes("http://x.org/AA"));
assert.ok(!cmd.classes().includes("http://x.org/A"));

// --- in-memory sssom convert (string -> string) ---
const sssomTsv =
  "#curie_map:\n#  X: http://ex/x/\n#  Y: http://ex/y/\n" +
  "#mapping_set_id: http://ex/ms\n" +
  "subject_id\tpredicate_id\tobject_id\tmapping_justification\n" +
  "X:1\tskos:exactMatch\tY:1\tsemapv:ManualMappingCuration\n";
const asJson = sssomConvert(sssomTsv, "json", "tsv");
assert.ok(asJson.includes("X:1") || asJson.includes("http://ex/x/1"));
assert.ok(sssomConvert(sssomTsv, "ttl", "tsv").length > 0);
assert.ok(sssomConvert(sssomTsv, "json").length > 0);  // from defaults to "tsv"

// --- MappingSet handle + records (tabular objects) ---
const ms = MappingSet.parse(sssomTsv, "tsv");
assert.equal(ms.size, 1);
const recs = ms.records();
assert.ok(Array.isArray(recs) && "subject_id" in recs[0]);
const ms2 = MappingSet.fromRecords(recs, ms.curieMap);
assert.ok(ms2.serialize("tsv").includes("subject_id"));
ms2.sort();
ms2.merge(MappingSet.parse(sssomTsv, "tsv"));
assert.equal(ms2.size, 2);
ms2.condense(); ms2.propagate();

// --- query records (array of objects) ---
const qrecs = cmd.queryRecords("SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 5");
assert.ok(Array.isArray(qrecs) && qrecs.length > 0 && "s" in qrecs[0]);

// --- DL query (Manchester syntax) ---
const dlOnt = Ontology.parse(
  enc.encode(
    "Prefix(:=<http://x/>)\nOntology(<http://x/o>\n" +
      "Declaration(Class(:Cell))\nDeclaration(Class(:Neuron))\nDeclaration(Class(:Brain))\n" +
      "Declaration(ObjectProperty(:part_of))\nSubClassOf(:Neuron :Cell)\n" +
      "SubClassOf(:Neuron ObjectSomeValuesFrom(:part_of :Brain))\n)\n",
  ),
  "ofn",
);
assert.ok(dlOnt.dlQuery("part_of some Brain", "descendants").includes("http://x/Neuron"));
assert.ok(dlOnt.dlQuery("Neuron", "ancestors").includes("http://x/Cell"));

// --- reasoned query: inferred subClassOf edges appear only with the reasoner ---
const eqOnt = Ontology.parse(
  enc.encode("Prefix(:=<http://x/>)\nOntology(<http://x/o>\nDeclaration(Class(:A))\nDeclaration(Class(:B))\nEquivalentClasses(:A :B)\n)\n"),
  "ofn",
);
const SCQ = "SELECT ?s ?o WHERE { ?s <http://www.w3.org/2000/01/rdf-schema#subClassOf> ?o }";
assert.equal(eqOnt.queryRecords(SCQ).length, 0);
assert.ok(eqOnt.queryRecords(SCQ, "elk").length > 0);   // reasoner adds inferred edges

// --- template table (first data row = directives) ---
const tont = new Ontology();
tont.template("ID\tLabel\tParent\nID\tLABEL\tSC %\nex:1\tThing One\tex:0\nex:2\tThing Two\tex:1\n");
assert.ok(tont.classes().length >= 2);

// --- DOSDP from a pattern + TSV data ---
const pattern = `pattern_name: t
classes: {thing: owl:Thing}
relations: {}
vars: {item: "'thing'"}
name: {text: "named %s", vars: [item]}
equivalentTo: {text: "'thing' and ('thing' some %s)", vars: [item]}
`;
const od = dosdp(pattern, "defined_class\titem\nex:1\tex:0\n");
assert.ok(od.axiomCount() > 0);

console.log("wasm smoke OK");
