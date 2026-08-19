# owlmake (Python)

Native Python bindings for [owlmake](../../README.md) — parse, reason over,
edit and serialize OWL ontologies in-process, with no Java, Docker or native
toolchain. Built with [pyo3](https://pyo3.rs); a single `abi3` wheel per
platform works on CPython 3.9+.

```python
from owlmake import Ontology, load, save

ont = load("hp.obo")                          # or Ontology.parse(data, "ofn")
ont.add_axioms("Declaration(Class(:D))\nSubClassOf(:D :A)")
ont.reason("elk")                             # built-in OWL 2 EL reasoner
for sub, sup in ont.subclass_pairs():
    ...
save(ont, "hp.owl")
```

## API

The `Ontology` method set mirrors the JavaScript (WebAssembly) package
one-for-one, so code reads the same across both languages.

| Member | Description |
| --- | --- |
| `Ontology.parse(bytes, format)` | Parse from `bytes`. |
| `Ontology()` | An empty ontology. |
| `serialize(format)` | Serialize to `bytes`. |
| `reason(reasoner)` | Classify and assert inferred axioms in place. |
| `reduce()` | Transitive reduction of the class hierarchy. |
| `relax()` | Relax equivalence/expression axioms to `SubClassOf`. |
| `merge(other)` | Merge another ontology into this one. |
| `add_axioms(ofn)` / `remove_axioms(ofn)` | Edit via OWL Functional-Syntax fragments. |
| `axiom_count()` / `len(ont)` | Component count. |
| `classes()` / `object_properties()` | Declared-entity IRIs. |
| `subclass_pairs()` | Named `SubClassOf` relations as `(sub, super)` tuples. |
| `filter(terms, select, signature)` / `remove(terms, select)` | Keep / drop axioms by term (ROBOT `filter`/`remove`). |
| `annotate(ontology_iri, version_iri, annotations)` | Set IRIs / add ontology annotations. |
| `rename(mapping)` | Bulk-rename entity IRIs from a dict. |
| `materialize(properties)` | Assert inferred existential restrictions. |
| `extract(terms, method)` | Extract a module (`BOT`/`TOP`/`STAR`/`MIREOT`) as a new ontology. |
| `diff(other)` | Human-readable diff against another ontology. |
| `measure()` | Ontology metrics, as `metric\tvalue` rows. |
| `query(sparql)` | SPARQL SELECT/ASK over the in-memory store, as TSV. |
| `load(path)` / `save(ont, path)` | File helpers (format inferred from extension). |

The command methods above run entirely in memory (no filesystem); they're the
same operations as the CLI commands, on a live `Ontology`.

One data command is an in-memory free function (string → string):

```python
owlmake.sssom_convert(tsv_text, "ttl")                  # SSSOM TSV -> Turtle
```

The other `sssom` subcommands remain available via the `sssom` sub-CLI, and
`jq` via the CLI dispatch (`owlmake.run("jq", ...)` / `owlmake.cli([...])`).

## pandas / polars

The tabular surfaces interoperate with both pandas and polars — a SSSOM
`MappingSet` round-trips with a DataFrame, SPARQL results come back as records,
and ROBOT `template` / DOSDP `dosdp` accept a DataFrame as their data table.

```python
from owlmake import Ontology, MappingSet, to_pandas, to_polars, mapping_set_from_dataframe

# SSSOM <-> DataFrame
ms = MappingSet.parse(open("mappings.sssom.tsv").read())
df = to_pandas(ms)                 # or to_polars(ms); both work
ms2 = mapping_set_from_dataframe(df, curie_map=ms.curie_map)
ms2.sort(); ms2.serialize("ttl")

# SPARQL results -> DataFrame
df = to_pandas(ont.query_records("SELECT ?s ?p ?o WHERE { ?s ?p ?o }"))

# Reason over the ontology and get the query result straight back as a DataFrame
# — the query sees inferred axioms (e.g. inferred subClassOf edges). The
# ontology itself is left unchanged.
df = ont.query_dataframe(
    "SELECT ?s ?o WHERE { ?s rdfs:subClassOf ?o }",
    reasoner="elk",            # or "hermit"; omit for the asserted graph
    backend="pandas",          # or "polars"
)

# DL query (like Protégé's DL Query tab): a Manchester-syntax class expression
# answered by the reasoner. kind = subclasses / descendants / superclasses /
# ancestors / equivalent / instances. Bare names resolve by label, CURIE,
# declared local name, then the default namespace.
neurons = ont.dl_query("part_of some 'brain'", "descendants", reasoner="elk")
df = ont.dl_query_dataframe("part_of some brain", "instances", backend="polars")

# ROBOT template from a DataFrame (first data row = the template strings)
ont.template(template_df)          # pandas or polars DataFrame, or TSV text

# DOSDP from a DataFrame data table
generated = owlmake.dosdp(pattern_yaml, data_df)   # -> Ontology
```

`MappingSet.records()` returns plain `list[dict]`, which both `pandas.DataFrame`
and `polars.DataFrame` ingest directly; `mapping_set_from_dataframe` reads back
from either (via `to_dict("records")` / `to_dicts()`). pandas and polars are
optional — only imported when you call the helpers.

## Commands

Beyond the object model, there is a typed function for **every** owlmake command
— one per ROBOT-style command and the full `sssom` sub-CLI — each running
**in-process** (no subprocess) through the same dispatch the `owlmake` binary
uses. Keyword arguments map to flags; the result is an `OwlmakeResult`
(`returncode` / `stdout` / `stderr`), raising `OwlmakeError` on non-zero exit
unless `raise_on_error=False`.

```python
import owlmake

owlmake.convert(input="hp.obo", output="hp.owl")
print(owlmake.measure(input="hp.owl").stdout)        # captured stdout
owlmake.sssom.convert("mappings.sssom.tsv", output="m.owl")

# ROBOT-style chaining through one in-memory ontology, in a single pass:
(owlmake.chain()
    .merge(input="edit.owl")
    .reason(reasoner="elk")
    .reduce()
    .convert(output="release.owl")
    .run())

# Escape hatch for anything not (yet) typed — orchestration (odk/make), new
# flags, raw chains. The module itself is callable as a shorthand:
owlmake("odk", "make", "release")
owlmake("reason -i a.obo -o b.owl")
```

The generated wrappers (`_commands.py`, `_sssom.py`) and the packaged spec
(`_spec.json`) are produced by `scripts/generate.py` from the CLI's own
`__cli-spec`, so they can't drift from the real flags; re-run it after changing
the Rust CLI.

## Reasoning

The external reasoner backends (`"hermit"`, `"jfact"`, `"whelk"`) are available
in this native build; `"elk"` / `"owlmake"` use owlmake's built-in OWL 2 EL
reasoner.

## Building

```sh
maturin build --release    # wheel in target/wheels/
maturin develop            # install into the active environment
```

The command wrappers are checked in (generated). After changing the Rust CLI,
regenerate them:

```sh
cargo build                       # refresh target/debug/om (the spec source)
python scripts/generate.py        # rewrites _commands.py / _sssom.py / _spec.json
```

Licensed under Apache-2.0.
