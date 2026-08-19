# owlmake (WebAssembly)

WebAssembly bindings for [owlmake](../../README.md) — parse, reason over,
edit and serialize OWL ontologies in Node and the browser, with no Java,
Docker or native toolchain.

A single `Ontology` handle wraps the full horned-owl object model plus
owlmake's operations. The method set mirrors the native Python binding
one-for-one; only the surface idiom differs (camelCase, `Uint8Array`,
native JS arrays).

Not yet published to npm — build the package from the repo (see
[Building](#building)) and install the generated `pkg/`:

```sh
wasm-pack build --target nodejs   # outputs ./pkg
npm install ./pkg
```

```js
import { Ontology } from "owlmake-wasm";

const ont = Ontology.parse(bytes, "ofn");   // "ofn" | "owl" | "obo" | "ttl" | …
ont.addAxioms("Declaration(Class(:D))\nSubClassOf(:D :A)");
ont.reason("elk");                          // built-in OWL 2 EL reasoner
const pairs = ont.subclassPairs();          // [[sub, super], …]
const out = ont.serialize("ofn");           // Uint8Array
```

## API

| Method | Description |
| --- | --- |
| `Ontology.parse(bytes, format)` | Parse from a `Uint8Array`. |
| `new Ontology()` | An empty ontology. |
| `serialize(format)` | Serialize to a `Uint8Array`. |
| `reason(reasoner)` | Classify and assert inferred axioms in place. |
| `reduce()` | Transitive reduction of the class hierarchy. |
| `relax()` | Relax equivalence/expression axioms to `SubClassOf`. |
| `merge(other)` | Merge another ontology into this one. |
| `addAxioms(ofn)` / `removeAxioms(ofn)` | Edit via OWL Functional-Syntax fragments. |
| `axiomCount()` | Component count. |
| `classes()` / `objectProperties()` | Declared-entity IRIs. |
| `subclassPairs()` | Named `SubClassOf` relations as `[sub, super]` pairs. |
| `filter(terms, select, signature)` / `remove(terms, select)` | Keep / drop axioms by term (ROBOT `filter`/`remove`). |
| `annotate(ontologyIri, versionIri, annotations)` | Set IRIs / add ontology annotations. |
| `materialize(properties)` | Assert inferred existential restrictions. |
| `extract(terms, method)` | Extract a module (`BOT`/`TOP`/`STAR`/`MIREOT`) as a new ontology. |
| `diff(other)` | Human-readable diff against another ontology. |
| `measure()` | Ontology metrics, as `metric\tvalue` rows. |
| `query(sparql)` | SPARQL SELECT/ASK over the in-memory store, as TSV. |

Free functions and a `MappingSet` handle cover the in-memory data commands:

```js
import { Ontology, MappingSet, sssomConvert, dosdp } from "owlmake-wasm";

sssomConvert(tsvText, "ttl");                         // SSSOM TSV -> Turtle ("from" defaults to "tsv")

// SSSOM mapping set as tabular records (array of {slot: value} objects)
const ms = MappingSet.parse(tsvText, "tsv");
ms.size;                                              // row count (getter)
const rows = ms.records();                            // -> [{subject_id, ...}, ...]
const ms2 = MappingSet.fromRecords(rows, ms.curieMap); // curieMap is a getter/setter property
ms2.condense();                                       // condense multi-valued slots
ms2.propagate();                                      // propagate set-level slots onto rows

// SPARQL results as records; pass a reasoner to query the entailed graph
const recs = ont.queryRecords("SELECT ?s ?p ?o WHERE { ?s ?p ?o }");
const inferred = ont.queryRecords("SELECT ?s ?o WHERE { ?s rdfs:subClassOf ?o }", "elk");

// DL query (like Protégé): a Manchester-syntax class expression, answered by the
// reasoner. kind = subclasses/descendants/superclasses/ancestors/equivalent/instances
const matches = ont.dlQuery("part_of some brain", "descendants");
ont.template(tsvText);                                // first data row = directives
const generated = dosdp(patternYaml, dataTsv);        // -> Ontology
```

These run entirely in memory (no filesystem).

`Ontology` and `MappingSet` are wasm-bindgen handles backed by Rust-side
memory that the JavaScript garbage collector does not track. They are reclaimed
when collected, but for tight loops or long-lived processes call `.free()` when
done (or use TypeScript 5.2+ `using` with explicit resource management) to
release the backing memory promptly. Using a handle after `free()` throws.

`rename` and RDF/XML parsing both work in the browser: horned-owl's RDF reader
uses a wasm-safe clock (`web-time`), so neither traps the module.

## Reasoning

The wasm build ships every reasoner backend: owlmake's built-in OWL 2 EL
reasoner (`"elk"` / `"owlmake"`), the hermit-rs OWL 2 DL reasoner (`"hermit"` /
`"jfact"`, full SROIQ(D)), and the whelk-rs OWL 2 EL reasoner (`"whelk"`,
matching ROBOT's Whelk). All three run in the browser — no fallback.

## Building

```sh
wasm-pack build --target nodejs   # or: web, bundler
```

`wasm-opt` is disabled in `Cargo.toml` (it fetches binaryen at build time);
re-enable it where the network allows for a smaller, faster artifact.

Licensed under Apache-2.0.
