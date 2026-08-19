# owlmake

owlmake is an ontology building stack implemented as a single portable Rust binary, `om`. It was designed to build the [Experimental Factor Ontology (EFO)](https://github.com/EBISPOT/EFO) and its dependencies performantly, on modest hardware, and without a VM/Docker. It can also build many of the ontologies from the OBO Foundry with identical results to the ODK builds.

owlmake uses [`horned-owl`](https://github.com/phillord/horned-owl) and [`whelk-rs`](https://github.com/INCATools/whelk-rs)

## Installation

1. Download the correct binary for your architecture from the [Releases page](https://github.com/EBISPOT/owlmake/releases). For example:

```
wget https://github.com/EBISPOT/owlmake/releases/download/v0.1.0/om-macos-arm64
```

2. Rename it to `om` and make sure it's on your `PATH` and executable. For example:

```
chmod +x om-macos-arm64
mv om-macos-arm64 ~/.local/bin/om
```
   
## Usage

### To build ontologies

From the repository of the ontology you want to build, run `om`. Targets will be discovered from the existing ODK setup (yaml/makefiles), then a plan will be generated in `owlmake.yaml` and executed. For example, to build the [Ontology of Biological Attributes (OBA)](https://github.com/obophenotype/bio-attribute-ontology):

```
git clone https://github.com/obophenotype/bio-attribute-ontology
cd bio-attribute-ontology
om
```

**Much of the existing ODK is functionally mirrored by the `om` binary itself (the majority of `robot`, `sssom`, `owltools`, `dosdp-tools`), so for many existing ontologies there are no environmental dependencies.** You can build ontologies like EFO, CL, and UBERON out of the box without Docker, Java, or Python.

Notable exceptions are ontologies using Python scripts as part of their build, e.g. uPheno which uses Python and Pandas which are not supplied as part of the single `om` binary. For this reason Docker images are also provided, as a compact alternative to the ODK image (2.81 GB for `odkfull`):

- the **default** image bundles just `om`, with `robot`/`jq`/`sssom` shims on the `PATH` — **~38 MB**. This builds every ontology `om` handles natively (EFO, CL, UBERON, …).
- the **`with-python`** image adds a Python 3 runtime plus Pandas (and NumPy) for the ontologies that shell out part of their builds to Python, and `git` for the recipes that diff against a release — **~200 MB**.

Both are published to GitHub Container Registry for `linux/amd64` and `linux/arm64`:

```
docker pull ghcr.io/ebispot/owlmake                # default (slim)
docker pull ghcr.io/ebispot/owlmake:latest-python  # with a Python runtime
```

Or build them yourself from this repo:

```
docker build -t owlmake .                              # default (slim)
docker build -t owlmake:python --target with-python .  # with a Python runtime
```

### Building specific targets

By default every release artefact is built. To build only some, name them as targets as in `make`. Targets are matched by filename, artefact name, or export format. For OBA:

```
om oba.owl               # the primary product
om oba-base.owl oba.obo  # the base module and the OBO export
om full simple           # by artefact name
```

### Refreshing imports

ODK ontologies pull external terms (PR, CHEBI, GO, …) into `imports/`. owlmake reuses the committed import modules by default; to re-mirror them from upstream and regenerate the modules, run the `refresh-imports` command (`--exclude-large` skips imports flagged `is_large_import`):

```
om refresh-imports                 # ODK `refresh-imports`
om refresh-imports --exclude-large # ODK `refresh-imports-excluding-large`
om all-imports                     # rebuild every individual import module
```

### ODK targets

ODK's standard Make targets are available as top-level owlmake commands:

```
om prepare-release   # build every release artefact (ODK `prepare_release`/`all`)
om refresh-imports   # rebuild imports from upstream
om test              # run the repository's QC checks
```

Any other target defined in the repo's Makefile is dispatched too — `om <target>` interprets that target's recipe (and its prerequisites). Targets owlmake doesn't replicate (`update_repo`, `clean`, `seed`-style scaffolding) report a clear error rather than doing the wrong thing.

### Starting a new ontology

`om seed` scaffolds a starter `owlmake.yaml` for a new ontology:

```
om seed --id myont
```

This writes a buildable plan (primary + base products and obo/json exports, built merge → reason → relax → reduce → annotate over `myont-edit.obo`). A repo defined only by a committed plan — no ODK Makefile or yaml — builds straight from it: `om` reads the plan as the source of truth (it is not regenerated). The plan is YAML by default; `om make --plan-format json` writes `owlmake.json` instead, and either spelling is accepted when building (commit both and they must describe the same build). Validate a plan against its schema with `om schema`.

### As a ROBOT implementation

`om` can be used as a drop in robot implementation for many commonly used commands. For example:

```
om reason -i oba-full.owl --reasoner elk -o reasoned.owl
om reduce -i reasoned.owl -o reduced.owl
om extract -i oba-full.owl --method BOT --term-file seed.txt -o module.owl
om convert -i oba.owl -o oba.obo
om diff --left oba-released.owl --right oba.owl
```

The *real* `robot` is not required for this, nor are Java, Python, or Docker.

## Language bindings (Python and JavaScript)

Everything owlmake does is also available as a library, through native
bindings that wrap the same Rust core (`owlmake::api`).

Python ([`crates/owlmake-py`](crates/owlmake-py), pyo3 — build and install from the repo with `pip install ./crates/owlmake-py`):

```python
from owlmake import Ontology, load, save

ont = load("hp.obo")
ont.add_axioms("Declaration(Class(:D))\nSubClassOf(:D :A)")
ont.reason("elk")                       # or "hermit" for full OWL 2 DL
for sub, sup in ont.subclass_pairs():
    ...
save(ont, "hp.owl")
```

```python
import owlmake
owlmake.convert(input="hp.obo", output="hp.owl")
owlmake.sssom.convert("mappings.sssom.tsv", output="m.owl")
(owlmake.chain().merge(input="edit.owl").reason(reasoner="elk").reduce()
    .convert(output="release.owl").run())
```

The tabular surfaces interoperate with **pandas and polars**: a SSSOM
`MappingSet` round-trips with a DataFrame, SPARQL results come back as records,
and ROBOT `template` / DOSDP accept a DataFrame as their data table.

```python
from owlmake import MappingSet, to_pandas, mapping_set_from_dataframe
df = to_pandas(MappingSet.parse(open("mappings.sssom.tsv").read()))   # or to_polars
ont.template(template_df)                       # pandas/polars DataFrame, or TSV
generated = owlmake.dosdp(pattern_yaml, data_df)

# Reason over the ontology and get the SPARQL result back as a DataFrame —
# inferred axioms included, the ontology left unchanged:
df = ont.query_dataframe("SELECT ?s ?o WHERE { ?s rdfs:subClassOf ?o }",
                         reasoner="elk", backend="pandas")   # or "polars"

# Or a Protégé-style DL query — a Manchester-syntax class expression answered by
# the reasoner — straight to a DataFrame:
df = ont.dl_query_dataframe("part_of some 'brain'", "descendants", reasoner="elk")
```

JavaScript / WebAssembly ([`crates/owlmake-wasm`](crates/owlmake-wasm),
wasm-bindgen — runs in Node and the browser):

```js
import { Ontology } from "owlmake";

const ont = Ontology.parse(bytes, "ofn");
ont.addAxioms("SubClassOf(:A :B)");
ont.reason("elk");
const out = ont.serialize("ofn");       // Uint8Array
```
